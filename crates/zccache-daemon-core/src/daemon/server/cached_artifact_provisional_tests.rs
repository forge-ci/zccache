//! Residency, access-state, migration and warm resolver regressions for #1965.

use super::*;

fn artifact(bytes: Arc<Vec<u8>>) -> ArtifactData {
    ArtifactData {
        outputs: vec![ArtifactOutput {
            name: "result.o".to_string(),
            payload: ArtifactPayload::Bytes(bytes),
        }],
        stdout: Arc::new(b"stdout".to_vec()),
        stderr: Arc::new(b"stderr".to_vec()),
        exit_code: 0,
    }
}

#[test]
fn demotion_releases_bytes_after_existing_hit_finishes_and_preserves_access() {
    let temp = tempfile::tempdir().unwrap();
    let bytes = Arc::new(b"owned payload".to_vec());
    let weak = Arc::downgrade(&bytes);
    let data = artifact(bytes);
    let provisional = ProvisionalArtifact::new(&data, vec![0o755]);
    let map = DashMap::new();
    let key = "7".repeat(64);
    let _retired = provisional.publish(&map, key.clone());
    let hit = map.get(&key).unwrap().clone();
    let now = Instant::now();
    hit.record_access(now);
    let before = hit.access_snapshot();
    persist_artifact_payloads(temp.path(), &key, &[weak.upgrade().unwrap()]).unwrap();
    drop(data);
    provisional.demote_to_durable(&map, &key);
    let durable = map.get(&key).unwrap().clone();
    assert!(!durable.same_instance(&hit));
    let after = durable.access_snapshot();
    assert_eq!(before.last_used, after.last_used);
    assert_eq!(before.last_used_wall, after.last_used_wall);
    assert_eq!(before.last_access_checkpoint, after.last_access_checkpoint);
    assert_eq!(before.used_in_process, after.used_in_process);
    assert_eq!(before.published_in_process, after.published_in_process);
    assert_eq!(durable.meta.output_modes, [0o755]);
    assert!(Arc::ptr_eq(&hit.stdout, &durable.stdout));
    drop(provisional);
    assert!(
        weak.upgrade().is_some(),
        "an existing hit still owns the bytes"
    );
    drop(hit);
    assert!(
        weak.upgrade().is_none(),
        "the durable map must not retain bytes"
    );
    let payloads = ensure_payloads_for_materialization(&durable, temp.path(), &key).unwrap();
    assert!(matches!(payloads[0], CachedPayload::File(_)));
}

#[test]
fn old_completion_does_not_resurrect_a_removed_entry() {
    let provisional = ProvisionalArtifact::new(&artifact(Arc::new(vec![1])), Vec::new());
    let map = DashMap::new();
    let _retired = provisional.publish(&map, "key".to_string());
    map.remove("key");
    provisional.demote_to_durable(&map, "key");
    assert!(map.is_empty());
}

#[test]
fn legacy_migration_publishes_only_durable_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let data = artifact(Arc::new(b"legacy payload".to_vec()));
    let key = "8".repeat(64);
    std::fs::write(
        temp.path().join(format!("{key}.meta")),
        bincode::serialize(&data).unwrap(),
    )
    .unwrap();
    let store = ArtifactStore::open_empty(&temp.path().join("index.bin"));
    let map = DashMap::new();
    let (count, retired) = migrate_meta_files(temp.path(), &map, &store);
    assert_eq!(count, 1);
    let trash = temp
        .path()
        .join(zccache_artifact::staged_lock::STAGED_ROOT)
        .join(".trash");
    assert!(
        std::fs::read_dir(trash).is_ok_and(|mut entries| entries.next().is_some()),
        "migration unlinked legacy metadata instead of returning detached cleanup"
    );
    let migrated = map.get(&key).unwrap().clone();
    let payloads = ensure_payloads_for_materialization(&migrated, temp.path(), &key).unwrap();
    assert!(matches!(payloads[0], CachedPayload::File(_)));
    drop(payloads);
    let report = retired.sweep_for(crate::core::wait::PROBE).unwrap();
    assert_eq!(report.failures, 0);
}

/// Differential latency assertion: both entries use the same warmed resolver;
/// 3x headroom absorbs scheduler noise without adding a second benchmark system.
#[test]
fn perf_demoted_warm_resolver_matches_existing_file_backed_budget() {
    let temp = tempfile::tempdir().unwrap();
    let data = artifact(Arc::new(vec![42; 4096]));
    let key = "9".repeat(64);
    persist_artifact_payloads(
        temp.path(),
        &key,
        &[Arc::clone(data.outputs[0].payload.as_bytes().unwrap())],
    )
    .unwrap();
    let provisional = ProvisionalArtifact::new(&data, Vec::new());
    let demoted = provisional.durable();
    let baseline = CachedArtifact::from_index(demoted.meta.clone());
    for cached in [&demoted, &baseline] {
        drop(ensure_payloads_for_materialization(cached, temp.path(), &key).unwrap());
    }
    let mut demoted_ns = 0;
    let mut baseline_ns = 0;
    for iteration in 0..256 {
        // Alternate the order to control for cache and scheduler effects.
        for is_demoted in [iteration % 2 == 0, iteration % 2 != 0] {
            let cached = if is_demoted { &demoted } else { &baseline };
            let start = Instant::now();
            let payloads = ensure_payloads_for_materialization(cached, temp.path(), &key).unwrap();
            assert!(matches!(payloads[0], CachedPayload::File(_)));
            drop(payloads);
            let elapsed_ns = start.elapsed().as_nanos();
            if is_demoted {
                demoted_ns += elapsed_ns;
            } else {
                baseline_ns += elapsed_ns;
            }
        }
    }
    let demoted_time = Duration::from_nanos(u64::try_from(demoted_ns).unwrap());
    let budget =
        Duration::from_nanos(u64::try_from(baseline_ns * 3).unwrap()) + Duration::from_millis(10);
    assert!(
        demoted_time <= budget,
        "demoted resolver {demoted_time:?} exceeds {budget:?}"
    );
}

#[test]
fn every_cache_hit_delivery_path_uses_the_daemon_shared_materialization_lease() {
    let consumers = [
        (
            "single compile",
            include_str!("handle_compile/cached_hit.rs"),
        ),
        (
            "multi compile",
            include_str!("handle_compile_multi_lookup.rs"),
        ),
        ("exact exec", include_str!("handle_exec.rs")),
        (
            "link and directory bundle",
            include_str!("link_cache_hit.rs"),
        ),
    ];
    for (name, source) in consumers {
        assert!(
            source.contains("ensure_payloads_for_materialization_for_state("),
            "{name} bypasses the typed staged materialization lease"
        );
    }
    assert!(
        include_str!("link_cache_hit.rs")
            .contains("materialize_directory_payload(&payloads[0], &output_path)"),
        "directory-bundle unpacking must consume payloads from the leased resolver"
    );
}
