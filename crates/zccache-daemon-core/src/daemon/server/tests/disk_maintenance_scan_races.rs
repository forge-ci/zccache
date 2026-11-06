//! A publisher renames and removes temporaries under the live store while a
//! maintenance scan walks it, so an entry can vanish between being listed and
//! being stat'ed. That entry is simply gone; only `NotFound` on an individual
//! entry is tolerated, and a missing root or any other error still surfaces.

use super::*;
use crate::daemon::server::persist::{
    persist_staged_artifact_payloads, scan_v2_disk_artifacts, StagedHookGuard, StagedHookPoint,
};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Runs `scan`, deleting `victim` at the moment the scan has listed it but not
/// yet stat'ed it, and returns the scan's result.
fn with_entry_removed_after_listing<T: Send + 'static>(
    victim: &Path,
    scan: impl FnOnce() -> T + Send + 'static,
) -> T {
    let hook = StagedHookGuard::arm(victim, StagedHookPoint::ScanEntryListed);
    let worker = std::thread::spawn(scan);
    hook.wait_until_reached();
    if victim.is_dir() {
        std::fs::remove_dir_all(victim).unwrap();
    } else {
        std::fs::remove_file(victim).unwrap();
    }
    hook.resume();
    worker.join().unwrap()
}

fn staged_fixture() -> (tempfile::TempDir, PathBuf, String, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let artifact_dir = root.path().to_path_buf();
    let key = "a".repeat(64);
    let staged_root = artifact_dir.join(".staged-v2");
    std::fs::create_dir_all(staged_root.join(&key)).unwrap();
    (root, artifact_dir, key, staged_root)
}

#[test]
fn legacy_file_removed_after_listing_is_not_an_artifact() {
    let root = tempfile::tempdir().unwrap();
    let artifact_dir = root.path().to_path_buf();
    let victim = artifact_dir.join("gone.meta");
    std::fs::write(&victim, b"x").unwrap();
    std::fs::write(artifact_dir.join("kept.meta"), b"y").unwrap();

    let scanned = with_entry_removed_after_listing(&victim, move || scan_artifacts(&artifact_dir))
        .expect("a vanished legacy file must not fail the scan");

    // A group for the vanished key would carry a UNIX_EPOCH last_access and
    // be planned for age expiry, so it must not exist at all.
    let keys: Vec<&str> = scanned
        .iter()
        .map(|artifact| artifact.key.as_str())
        .collect();
    assert_eq!(keys, ["kept"]);
}

#[test]
fn staged_pointer_temporary_removed_after_listing_is_skipped() {
    let (_root, artifact_dir, key, staged_root) = staged_fixture();
    let victim = staged_root.join(format!(".{key}.current.1.2-tmp"));
    std::fs::write(&victim, b"generation").unwrap();

    let scanned =
        with_entry_removed_after_listing(&victim, move || scan_v2_disk_artifacts(&artifact_dir))
            .expect("a renamed pointer temporary must not fail the scan");

    assert_eq!(scanned.len(), 1);
    assert_eq!(scanned[0].key, key);
}

#[test]
fn staged_key_root_removed_after_listing_is_not_an_artifact() {
    let (_root, artifact_dir, key, staged_root) = staged_fixture();
    let victim = staged_root.join(&key);

    let scanned =
        with_entry_removed_after_listing(&victim, move || scan_v2_disk_artifacts(&artifact_dir))
            .expect("a removed key root must not fail the scan");

    assert!(scanned.is_empty());
}

#[test]
fn staged_temporary_generation_renamed_after_listing_is_skipped() {
    let (_root, artifact_dir, key, staged_root) = staged_fixture();
    let key_root = staged_root.join(&key);
    std::fs::write(key_root.join("kept"), b"k").unwrap();
    let victim = key_root.join(".tmp-1-2");
    std::fs::create_dir(&victim).unwrap();
    std::fs::write(victim.join("output"), b"o").unwrap();

    let scanned =
        with_entry_removed_after_listing(&victim, move || scan_v2_disk_artifacts(&artifact_dir))
            .expect("a renamed temporary generation must not fail the scan");

    assert_eq!(scanned.len(), 1);
}

#[test]
fn accounting_walk_skips_a_file_removed_after_listing() {
    let root = tempfile::tempdir().unwrap();
    let tree = root.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    std::fs::write(tree.join("kept"), vec![0_u8; 4096]).unwrap();
    let victim = tree.join("gone");
    std::fs::write(&victim, b"x").unwrap();

    let (result, artifact) = with_entry_removed_after_listing(&victim, move || {
        let mut artifact = artifact("key", 0, SystemTime::now(), Duration::ZERO);
        let result = add_tree(&tree, &mut artifact, &mut HashSet::new());
        (result, artifact)
    });

    result.expect("a vanished file must not fail the accounting walk");
    assert!(
        artifact.allocated_bytes > 0,
        "the surviving file still counts"
    );
}

/// The production shape, without an injected pause: real publications racing
/// real scans over the live store. Bounded by attempts, not a wall-clock wait.
#[test]
fn scans_survive_concurrent_staged_publication() {
    let root = tempfile::tempdir().unwrap();
    let artifact_dir = root.path().to_path_buf();
    let stop = Arc::new(AtomicBool::new(false));
    let publisher = {
        let artifact_dir = artifact_dir.clone();
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut round = 0_u32;
            while !stop.load(Ordering::Acquire) {
                // One payload per key: a key republished with different bytes
                // fails closed by design, which is not what this exercises.
                let key_index = round % 64;
                let key = format!("{key_index:064x}");
                let payload = Arc::new(key_index.to_le_bytes().to_vec());
                persist_staged_artifact_payloads(&artifact_dir, &key, &[payload]).unwrap();
                round += 1;
            }
        })
    };
    let mut outcome = Ok(());
    for _ in 0..500 {
        outcome = scan_artifacts(&artifact_dir).map(|_| ());
        if outcome.is_err() {
            break;
        }
    }
    stop.store(true, Ordering::Release);
    publisher.join().unwrap();
    outcome.expect("a scan racing publication must not fail");
}

#[test]
fn missing_artifact_root_still_surfaces() {
    let root = tempfile::tempdir().unwrap();
    let error = validate_owned_artifact_root(&root.path().join("missing")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

#[test]
fn only_not_found_is_tolerated_on_an_entry() {
    use crate::daemon::server::persist::entry_if_present;
    let vanished = entry_if_present::<()>(Err(io::ErrorKind::NotFound.into())).unwrap();
    assert_eq!(vanished, None);
    let denied = entry_if_present::<()>(Err(io::ErrorKind::PermissionDenied.into())).unwrap_err();
    assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied);
}

/// Hitting the vanish path must not turn a non-`NotFound` failure silent: a
/// path through a regular file is `NotADirectory`, not a vanished entry.
#[cfg(unix)]
#[test]
fn accounting_walk_surfaces_errors_other_than_not_found() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let mut artifact = artifact("key", 0, SystemTime::now(), Duration::ZERO);
    let error = add_file(&file.join("child"), &mut artifact, &mut HashSet::new()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
}
