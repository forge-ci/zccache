//! #1964: recursive cleanup must not retain exclusion from publishers.

use super::*;
use std::time::Duration;

#[test]
fn maintenance_sweep_allows_unrelated_publication_after_detach() {
    publication_while_sweeping(false);
}

#[test]
fn clear_sweep_allows_unrelated_publication_after_detach() {
    publication_while_sweeping(true);
}

#[test]
fn publication_conflict_preserves_a_retained_reader_path_until_its_lease_ends() {
    use super::super::{
        acquire_staged_materialization_guard_if_present, cleanup_staged_artifact_temps,
        load_staged_artifact_paths, persist_staged_artifact_paths,
    };
    let directory = tempfile::tempdir().unwrap();
    let artifact_dir = directory.path().join("artifacts");
    fs::create_dir_all(&artifact_dir).unwrap();
    let source = directory.path().join("source");
    let key = "c".repeat(64);
    fs::write(&source, b"old").unwrap();
    let sources = vec![crate::core::NormalizedPath::from(source.clone())];
    persist_staged_artifact_paths(&artifact_dir, &key, &sources).unwrap();
    let lease = acquire_staged_materialization_guard_if_present(&artifact_dir, &key)
        .unwrap()
        .unwrap();
    let old_path = load_staged_artifact_paths(&artifact_dir, &key, &[3])
        .unwrap()
        .unwrap()
        .remove(0);
    fs::write(&source, b"new").unwrap();
    assert!(persist_staged_artifact_paths(&artifact_dir, &key, &sources).is_err());
    assert!(
        !pointer_path(&artifact_dir, &key).exists(),
        "a conflicting key must stop serving new hits"
    );
    assert_eq!(
        fs::read(&old_path).expect("conflict cleanup invalidated a retained reader"),
        b"old"
    );
    drop(lease);
    cleanup_staged_artifact_temps(&artifact_dir).unwrap();
    assert!(
        !old_path.exists(),
        "unreferenced generation survived post-lease recovery"
    );
}

#[test]
fn failed_staged_sweep_is_retried_from_durable_trash() {
    use super::super::{
        detach_staged_artifacts_for_clear, persist_staged_artifact_paths, sweep_staged_trash_for,
        StagedFaultGuard, StagedFaultPoint,
    };
    let directory = tempfile::tempdir().unwrap();
    let artifact_dir = directory.path().join("artifacts");
    fs::create_dir_all(&artifact_dir).unwrap();
    let source = directory.path().join("source");
    fs::write(&source, b"recover this payload").unwrap();
    persist_staged_artifact_paths(&artifact_dir, &"d".repeat(64), &[source.into()]).unwrap();
    let detached =
        detach_staged_artifacts_for_clear(&artifact_dir, Duration::from_secs(5)).unwrap();
    let fault = StagedFaultGuard::arm(&artifact_dir, [StagedFaultPoint::TrashDelete]);
    let first = detached.sweep_for(Duration::from_secs(5)).unwrap();
    fault.assert_all_consumed();
    assert!(first.failures > 0);
    let retry = sweep_staged_trash_for(&artifact_dir, Duration::from_secs(5)).unwrap();
    assert_eq!(retry.failures, 0);
    assert!(retry.reclaimed_bytes > 0);
    let leftovers =
        fs::read_dir(staged_root(&artifact_dir).join(super::super::trash::STAGED_TRASH))
            .map(|entries| entries.count())
            .unwrap_or_else(|error| {
                assert_eq!(error.kind(), io::ErrorKind::NotFound);
                0
            });
    assert_eq!(leftovers, 0);
}

#[test]
fn an_expired_sweep_leaves_recoverable_names() {
    use super::super::{
        detach_staged_artifacts_for_clear, persist_staged_artifact_paths, sweep_staged_trash_for,
    };
    let directory = tempfile::tempdir().unwrap();
    let artifact_dir = directory.path().join("artifacts");
    fs::create_dir_all(&artifact_dir).unwrap();
    let source = directory.path().join("source");
    fs::write(&source, b"deferred payload").unwrap();
    persist_staged_artifact_paths(&artifact_dir, &"e".repeat(64), &[source.into()]).unwrap();
    let detached =
        detach_staged_artifacts_for_clear(&artifact_dir, Duration::from_secs(5)).unwrap();
    let report = detached.sweep_for(Duration::ZERO).unwrap();
    assert!(report.deferred);
    assert_eq!(report.reclaimed_bytes, 0);
    let recovered = sweep_staged_trash_for(&artifact_dir, Duration::from_secs(5)).unwrap();
    assert_eq!(recovered.failures, 0);
    assert!(recovered.reclaimed_bytes > 0);
}

#[cfg(any(unix, windows))]
#[test]
fn a_linked_trash_root_is_refused_before_any_live_name_is_detached() {
    use super::super::{
        detach_staged_artifacts_for_clear, persist_staged_artifact_paths, sweep_staged_trash_for,
    };
    let directory = tempfile::tempdir().unwrap();
    let artifact_dir = directory.path().join("artifacts");
    fs::create_dir_all(&artifact_dir).unwrap();
    let source = directory.path().join("source");
    fs::write(&source, b"preserve staged payload").unwrap();
    let key = "f".repeat(64);
    persist_staged_artifact_paths(&artifact_dir, &key, &[source.into()]).unwrap();
    let outside = directory.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("sentinel");
    fs::write(&sentinel, b"do not touch").unwrap();
    let trash = staged_root(&artifact_dir).join(super::super::trash::STAGED_TRASH);
    directory_link(&outside, trash.as_path());
    let error = match detach_staged_artifacts_for_clear(&artifact_dir, Duration::from_secs(2)) {
        Err(error) => error,
        Ok(_) => panic!("linked trash must not receive live staged names"),
    };
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(pointer_path(&artifact_dir, &key).exists());
    assert!(staged_root(&artifact_dir).join(&key).exists());
    assert!(sweep_staged_trash_for(&artifact_dir, Duration::from_secs(2)).is_err());
    assert_eq!(fs::read(&sentinel).unwrap(), b"do not touch");
}

#[cfg(any(unix, windows))]
#[test]
fn a_link_inside_trash_is_unlinked_without_traversing_its_target() {
    use super::super::sweep_staged_trash_for;
    let directory = tempfile::tempdir().unwrap();
    let artifact_dir = directory.path().join("artifacts");
    let trash = staged_root(&artifact_dir).join(super::super::trash::STAGED_TRASH);
    fs::create_dir_all(&trash).unwrap();
    let outside = directory.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("sentinel");
    fs::write(&sentinel, b"retained outside target").unwrap();
    let link = trash.join("linked-entry");
    directory_link(&outside, link.as_path());
    let report = sweep_staged_trash_for(&artifact_dir, Duration::from_secs(2)).unwrap();
    assert_eq!(report.failures, 0);
    assert_eq!(report.reclaimed_bytes, 0);
    assert!(!link.exists());
    assert_eq!(fs::read(&sentinel).unwrap(), b"retained outside target");
}

#[cfg(unix)]
fn directory_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn directory_link(target: &Path, link: &Path) {
    let result = crate::core::wait::output_for(
        kernal_api::SpawnSpec::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(link.as_os_str())
            .arg(target.as_os_str()),
        Duration::from_secs(5),
    )
    .expect("create actual directory junction for the reparse regression");
    assert_eq!(
        result.exit.raw_code(),
        0,
        "junction creation failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn publication_while_sweeping(clear: bool) {
    use super::super::{persist_staged_artifact_paths, StagedHookGuard, StagedHookPoint};
    let directory = tempfile::tempdir().unwrap();
    let artifact_dir = directory.path().join("artifacts");
    fs::create_dir_all(&artifact_dir).unwrap();
    let source = directory.path().join("source");
    fs::write(&source, b"immutable staged payload").unwrap();
    let sources = vec![crate::core::path::NormalizedPath::from(source)];
    let old_key = "a".repeat(64);
    let new_key = "b".repeat(64);
    persist_staged_artifact_paths(&artifact_dir, &old_key, &sources).unwrap();
    let expected = HashMap::from([(
        old_key.clone(),
        Some(
            fs::read_to_string(pointer_path(&artifact_dir, &old_key))
                .unwrap()
                .trim()
                .to_string(),
        ),
    )]);
    let hook = StagedHookGuard::arm(&artifact_dir, StagedHookPoint::TreeSweep);
    let cleaner_dir = artifact_dir.clone();
    let cleaner = std::thread::spawn(move || {
        if clear {
            clear_staged_artifacts(&cleaner_dir).map(|_| ())
        } else {
            evict_staged_artifact_keys_if_unchanged(&cleaner_dir, &expected).map(|_| ())
        }
    });
    hook.wait_until_reached_for(Duration::from_secs(10));
    let detached_before_sweep = !staged_root(&artifact_dir).join(&old_key).exists()
        && !pointer_path(&artifact_dir, &old_key).exists();
    let native_available =
        crate::artifact::StagedReadGuard::acquire_for(&artifact_dir, Duration::ZERO).is_ok();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    let publisher_dir = artifact_dir.clone();
    let publisher_key = new_key.clone();
    let publisher = std::thread::spawn(move || {
        let result = persist_staged_artifact_paths(&publisher_dir, &publisher_key, &sources);
        crate::core::wait::send_for(&done_tx, result, Duration::from_secs(5)).unwrap();
    });
    let published = crate::core::wait::recv_for(&done_rx, Duration::from_secs(1));
    let published_before_resume = published.is_ok();
    let publication_started = staged_root(&artifact_dir).join(&new_key).exists();
    // Release and drain both workers even on the baseline failure path.
    hook.resume();
    let result = published.unwrap_or_else(|_| {
        crate::core::wait::recv_for(&done_rx, Duration::from_secs(10)).unwrap()
    });
    result.unwrap();
    crate::core::wait::join_for(publisher, Duration::from_secs(10))
        .unwrap()
        .unwrap();
    crate::core::wait::join_for(cleaner, Duration::from_secs(10))
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        native_available,
        "paused recursive cleanup retained the native store lock"
    );
    assert!(
        published_before_resume,
        "publication did not finish during paused cleanup: native_available={native_available}, publication_started={publication_started}"
    );
    assert!(
        detached_before_sweep,
        "evicted entries were reachable when recursive cleanup began"
    );
    assert!(
        pointer_path(&artifact_dir, &new_key).exists(),
        "Clear swept a new publication"
    );
}

#[test]
fn process_exit_after_retirement_is_recovered_without_erasing_a_replacement() {
    use super::super::{
        load_staged_artifact_paths, persist_staged_artifact_paths, sweep_staged_trash_for,
    };
    let directory = tempfile::tempdir().unwrap();
    let artifact_dir = directory.path().join("artifacts");
    fs::create_dir_all(&artifact_dir).unwrap();
    let output = crate::core::wait::output_for(
        kernal_api::SpawnSpec::new(std::env::current_exe().unwrap())
            .args(["--exact", "daemon::server::persist::staged_store::maintenance::sweep_tests::retirement_exit_child", "--ignored", "--nocapture"])
            .env("ZCCACHE_RETIREMENT_EXIT_CHILD", artifact_dir.as_os_str()),
        Duration::from_secs(10),
    ).unwrap();
    assert_eq!(
        output.exit.raw_code(),
        86,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let key = "f".repeat(64);
    assert!(!pointer_path(&artifact_dir, &key).exists());
    assert!(fs::read_dir(staged_root(&artifact_dir).join(".trash"))
        .unwrap()
        .next()
        .is_some());
    let replacement = directory.path().join("replacement");
    fs::write(&replacement, b"fresh replacement").unwrap();
    persist_staged_artifact_paths(&artifact_dir, &key, &[replacement.into()]).unwrap();
    let report = sweep_staged_trash_for(&artifact_dir, Duration::from_secs(5)).unwrap();
    assert_eq!(report.failures, 0);
    assert!(!report.deferred);
    let paths = load_staged_artifact_paths(&artifact_dir, &key, &[17])
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(&paths[0]).unwrap(), b"fresh replacement");
    assert!(!staged_root(&artifact_dir).join(".trash").exists());
}

#[test]
#[ignore = "bounded subprocess fixture, invoked by process_exit_after_retirement test"]
fn retirement_exit_child() {
    use super::super::{detach_staged_artifacts_for_clear, persist_staged_artifact_paths};
    let Some(root) = std::env::var_os("ZCCACHE_RETIREMENT_EXIT_CHILD") else {
        return;
    };
    let artifact_dir = Path::new(&root);
    let source = artifact_dir.parent().unwrap().join("crashed-source");
    fs::write(&source, b"unreachable predecessor").unwrap();
    persist_staged_artifact_paths(artifact_dir, &"f".repeat(64), &[source.into()]).unwrap();
    let _retired = detach_staged_artifacts_for_clear(artifact_dir, Duration::from_secs(2)).unwrap();
    // A real process exit skips Rust destructors and releases native file locks.
    // The parent reopens the same store, publishes again, and retries recovery.
    std::process::exit(86);
}

#[test]
fn eviction_revalidates_a_generation_replaced_while_commit_waits() {
    use super::super::{
        load_staged_artifact_paths, persist_staged_artifact_paths, StagedHookGuard, StagedHookPoint,
    };
    let directory = tempfile::tempdir().unwrap();
    let artifact_dir = directory.path().join("artifacts");
    fs::create_dir_all(&artifact_dir).unwrap();
    let source = directory.path().join("source");
    let key = "e".repeat(64);
    fs::write(&source, b"old").unwrap();
    let sources = [crate::core::NormalizedPath::new(&source)];
    persist_staged_artifact_paths(&artifact_dir, &key, &sources).unwrap();
    let old = scan_staged_disk_artifacts(&artifact_dir)
        .unwrap()
        .remove(0)
        .generation;
    let expected = HashMap::from([(key.clone(), old.clone())]);
    let hook = StagedHookGuard::arm(&artifact_dir, StagedHookPoint::MaintenanceStoreLockPending);
    let worker_dir = artifact_dir.clone();
    let worker = std::thread::spawn(move || {
        detach_staged_artifact_keys_if_unchanged(&worker_dir, &expected, Duration::from_secs(2))
    });
    hook.wait_until_reached_for(Duration::from_secs(10));
    fs::write(&source, b"new").unwrap();
    // The first nondeterministic replacement invalidates the old key. A later
    // publication may then establish a fresh generation under that same key.
    assert!(persist_staged_artifact_paths(&artifact_dir, &key, &sources).is_err());
    persist_staged_artifact_paths(&artifact_dir, &key, &sources).unwrap();
    let new = fs::read_to_string(pointer_path(&artifact_dir, &key)).unwrap();
    assert_ne!(old.as_deref(), Some(new.trim()));
    hook.resume();
    let detached = crate::core::wait::join_for(worker, Duration::from_secs(5))
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        detached.removed.is_empty(),
        "stale scan retired a replacement generation"
    );
    detached.trash.sweep_for(Duration::from_secs(2)).unwrap();
    let paths = load_staged_artifact_paths(&artifact_dir, &key, &[3])
        .unwrap()
        .unwrap();
    assert_eq!(fs::read(&paths[0]).unwrap(), b"new");
}

#[test]
fn generation_retirement_refuses_oversized_pointer_under_commit() {
    let directory = tempfile::tempdir().unwrap();
    let artifact_dir = directory.path();
    let key = "a".repeat(64);
    let generation = "b".repeat(64);
    let old_generation = super::super::generation_dir(artifact_dir, &key, &generation);
    fs::create_dir_all(&old_generation).unwrap();
    fs::write(pointer_path(artifact_dir, &key), vec![b'c'; 4096]).unwrap();
    let result = detach_generation_if_unreferenced_for(
        artifact_dir,
        &key,
        &generation,
        Duration::from_secs(1),
    );
    assert!(matches!(result, Err(ref error) if error.kind() == io::ErrorKind::InvalidData));
    assert!(
        old_generation.exists(),
        "invalid pointer must not retire a generation"
    );
}

#[test]
fn clear_expired_enumeration_budget_keeps_live_names() {
    let directory = tempfile::tempdir().unwrap();
    let artifact_dir = directory.path().join("artifacts");
    fs::create_dir_all(&artifact_dir).unwrap();
    let source = directory.path().join("source");
    fs::write(&source, b"retained").unwrap();
    let key = "e".repeat(64);
    super::super::persist_staged_artifact_paths(
        &artifact_dir,
        &key,
        &[crate::core::NormalizedPath::from(source)],
    )
    .unwrap();
    let pointer = pointer_path(&artifact_dir, &key);
    let result = detach_staged_artifacts_for_clear(&artifact_dir, Duration::ZERO);
    assert!(matches!(result, Err(ref error) if error.kind() == io::ErrorKind::TimedOut));
    assert!(
        pointer.exists(),
        "expired enumeration must not detach live names"
    );
}
