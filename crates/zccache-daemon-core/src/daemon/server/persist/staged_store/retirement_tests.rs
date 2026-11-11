//! Corruption retirement, ownership races and temporary publication contracts.

use super::*;

fn poison(root: &Path) -> NormalizedPath {
    let blob = NormalizedPath::new(root).join("blob");
    fs::write(&blob, b"original").unwrap();
    write_authoritative_blob_digest(&blob).unwrap();
    fs::write(&blob, b"poisoned").unwrap();
    blob
}

#[test]
fn corruption_retirement_defers_for_publication_and_store_readers() {
    let root = tempfile::tempdir().unwrap();
    let blob = poison(root.path());
    let alias = root.path().join("alias");
    fs::hard_link(&blob, &alias).unwrap();
    let publication = crate::core::sync::PublicationBarrier::new();
    let reader = publication.try_read_owned().unwrap();
    let store_reader = StagedReadGuard::acquire_for(root.path(), crate::core::wait::PROBE).unwrap();
    queue_corrupt_blob(root.path(), &blob);
    maintain_pending_retirements_for(root.path(), &publication).unwrap();
    assert!(blob.exists());
    drop(reader);
    let waiting = Instant::now();
    maintain_pending_retirements_for(root.path(), &publication).unwrap();
    assert!(
        waiting.elapsed() < Duration::from_secs(3),
        "maintenance exceeded its two-second acquisition allowance"
    );
    assert!(
        blob.exists(),
        "cross-process reader must retain the cache name"
    );
    drop(store_reader);
    maintain_pending_retirements_for(root.path(), &publication).unwrap();
    assert!(!blob.exists());
    assert_eq!(fs::read(alias).unwrap(), b"poisoned");
}

#[test]
fn stale_corruption_snapshot_preserves_a_replacement() {
    let root = tempfile::tempdir().unwrap();
    let blob = poison(root.path());
    let snapshot = CorruptSnapshot::prepare(root.path(), &blob)
        .unwrap()
        .unwrap();
    persist_artifact_output(&blob, b"replacement").unwrap();
    let publication = crate::core::sync::PublicationBarrier::new();
    let mut trash = trash::DetachedStagedTrash::empty(root.path());
    let writer = publication.try_write().unwrap();
    let mut prepared = Prepared::Corrupt(snapshot);
    commit(root.path(), &mut prepared, &mut trash).unwrap();
    drop(writer);
    drop(prepared);
    trash.sweep_for(crate::core::wait::PROBE).unwrap();
    assert_eq!(fs::read(&blob).unwrap(), b"replacement");
    verify_registered_blob(&blob).unwrap();
}

#[test]
fn replacement_preserves_a_sealed_hardlink_and_its_open_identity() {
    use std::io::Read;
    let root = tempfile::tempdir().unwrap();
    let blob = root.path().join("blob");
    persist_artifact_output(&blob, b"original").unwrap();
    let alias = root.path().join("alias");
    fs::hard_link(&blob, &alias).unwrap();
    crate::platform::fs::permissions::seal_cache_blob(&blob).unwrap();
    let mut original = fs::File::open(&blob).unwrap();

    persist_artifact_output(&blob, b"replacement").unwrap();

    let mut pinned_bytes = Vec::new();
    original.read_to_end(&mut pinned_bytes).unwrap();
    assert_eq!(pinned_bytes, b"original");
    assert_eq!(fs::read(&alias).unwrap(), b"original");
    assert!(crate::platform::fs::permissions::is_sealed(&alias).unwrap());
    assert_eq!(fs::read(&blob).unwrap(), b"replacement");
    verify_registered_blob(&blob).unwrap();
}

#[test]
fn rollback_preserves_a_newer_sidecar_with_identical_digest_bytes() {
    let root = tempfile::tempdir().unwrap();
    let blob = root.path().join("blob");
    fs::write(&blob, b"published").unwrap();
    let published = super::super::super::link_registry::write_authoritative_blob_digest_for_pinned(
        &blob, &blob,
    )
    .unwrap();
    queue_digest_rollback(&blob, published, None);
    write_authoritative_blob_digest(&blob).unwrap();
    maintain_pending_retirements_for(root.path(), &crate::core::sync::PublicationBarrier::new())
        .unwrap();
    verify_registered_blob(&blob).unwrap();
    assert!(zccache_artifact::blob_digest::sidecar_path(&blob).exists());
}

#[test]
fn failed_publication_restores_the_old_digest_after_exclusion_release() {
    let root = tempfile::tempdir().unwrap();
    let blob = root.path().join("blob");
    fs::write(&blob, b"previous").unwrap();
    write_authoritative_blob_digest(&blob).unwrap();
    let previous = fs::read(zccache_artifact::blob_digest::sidecar_path(&blob)).unwrap();
    let failed = root.path().join("unpublished");
    fs::write(&failed, b"never published").unwrap();
    let published = super::super::super::link_registry::write_authoritative_blob_digest_for_pinned(
        &failed, &blob,
    )
    .unwrap();
    queue_digest_rollback(&blob, published, Some(previous));
    maintain_pending_retirements_for(root.path(), &crate::core::sync::PublicationBarrier::new())
        .unwrap();
    verify_registered_blob(&blob).unwrap();
    assert_eq!(fs::read(blob).unwrap(), b"previous");
}

#[test]
fn corrupt_staged_generation_is_retired_before_same_generation_republication() {
    let root = tempfile::tempdir().unwrap();
    let key = "a".repeat(64);
    let payload = Arc::new(b"original".to_vec());
    persist_staged_artifact_payloads(root.path(), &key, &[Arc::clone(&payload)]).unwrap();
    let paths = load_staged_artifact_paths(root.path(), &key, &[8])
        .unwrap()
        .unwrap();
    let blob = &paths[0];
    crate::platform::fs::permissions::make_writable(blob).unwrap();
    fs::write(blob, b"poisoned").unwrap();
    queue_corrupt_blob(root.path(), blob);
    maintain_pending_retirements_for(root.path(), &crate::core::sync::PublicationBarrier::new())
        .unwrap();
    assert!(!blob.exists());
    assert!(!pointer_path(root.path(), &key).exists());
    persist_staged_artifact_payloads(root.path(), &key, &[payload]).unwrap();
    let restored = load_staged_artifact_paths(root.path(), &key, &[8])
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(&restored[0]).unwrap(), b"original");
}
#[test]
fn repointed_orphan_generation_is_preserved_by_a_stale_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let key = "b".repeat(64);
    persist_staged_artifact_payloads(root.path(), &key, &[Arc::new(b"original".to_vec())]).unwrap();
    let paths = load_staged_artifact_paths(root.path(), &key, &[8])
        .unwrap()
        .unwrap();
    let blob = &paths[0];
    let generation = blob
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    crate::platform::fs::permissions::make_writable(blob).unwrap();
    fs::write(blob, b"poisoned").unwrap();
    let pointer = pointer_path(root.path(), &key);
    atomic_write(&pointer, "c".repeat(64).as_bytes()).unwrap();
    let snapshot = CorruptSnapshot::prepare(root.path(), blob)
        .unwrap()
        .unwrap();
    atomic_write(&pointer, generation.as_bytes()).unwrap();
    let publication = crate::core::sync::PublicationBarrier::new();
    let writer = publication.try_write().unwrap();
    let mut trash = trash::DetachedStagedTrash::empty(root.path());
    let mut prepared = Prepared::Corrupt(snapshot);
    commit(root.path(), &mut prepared, &mut trash).unwrap();
    drop(writer);
    drop(prepared);
    trash.sweep_for(crate::core::wait::PROBE).unwrap();
    assert!(
        blob.exists(),
        "a newly current generation requires a fresh retirement decision"
    );
    assert_eq!(fs::read_to_string(pointer).unwrap(), generation);
}

#[test]
fn owned_temporary_keeps_the_reflink_store_tier_available() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::write(&source, b"copy-on-write payload").unwrap();
    let probe = root.path().join("probe");
    let reflink_supported = kernal_api::platform::fs::reflink_file(&source, &probe).is_ok();
    let cached = root.path().join("cached");
    let stats = persist_artifact_file(&cached, &source, MaterializationMode::Auto).unwrap();
    if reflink_supported {
        assert_eq!(
            stats.reflink_count, 1,
            "the owned temporary must not disable reflinks"
        );
    }
    assert_eq!(fs::read(cached).unwrap(), b"copy-on-write payload");
}
#[cfg(unix)]
#[test]
fn fresh_temporary_normalizes_before_exclusive_creation() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("other/inner")).unwrap();
    std::os::unix::fs::symlink(root.path().join("other/inner"), root.path().join("alias")).unwrap();
    let parent = root.path().join("alias/..");
    let (_temporary, destination) = OutputDestination::fresh_temporary(&parent).unwrap();
    assert!(
        destination.as_ref().exists(),
        "authority must name the file actually created"
    );
    assert_eq!(destination.as_ref().parent(), Some(root.path()));
}

#[tokio::test]
async fn state_aware_delivery_queues_retirement_without_deleting_under_its_lease() {
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(|| {
            let directory = tempfile::tempdir().unwrap();
            let server = crate::daemon::server::tests::bind_isolated_server(directory.path());
            let state = &server.state;
            fs::create_dir_all(&state.artifact_dir).unwrap();
            let blob = poison(&state.artifact_dir);
            let outputs = tempfile::tempdir().unwrap();
            let requested = outputs.path().join("requested");
            let reader = state.artifact_publication.try_read_owned().unwrap();
            let result = write_payloads_par_observed_for_state(
                state,
                &[requested.as_path()],
                &[CachedPayload::File(blob.clone())],
                MaterializationMode::Copy,
            );
            assert!(matches!(
                result,
                Err(MaterializationFailure::CacheBlobMissing(_))
            ));
            assert!(!requested.exists());
            assert!(has_pending_retirements(&state.artifact_dir));
            maintain_pending_retirements(state).unwrap();
            assert!(blob.exists());
            drop(reader);
            maintain_pending_retirements(state).unwrap();
            assert!(!blob.exists());
        }),
    )
    .await
    .unwrap()
    .unwrap();
}
