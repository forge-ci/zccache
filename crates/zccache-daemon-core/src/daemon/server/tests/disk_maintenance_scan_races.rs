//! A publisher renames and removes temporaries under the live store while a
//! maintenance scan walks it, so an entry can vanish between being listed and
//! being stat'ed. That entry is simply gone; only `NotFound` on an individual
//! entry is tolerated, and a missing root or any other error still surfaces.
//! Windows can report the same race as access denied, so that is retried until
//! the entry resolves, but it never counts as vanished by itself.

use super::*;
use crate::daemon::server::persist::{
    persist_staged_artifact_payloads, scan_v2_disk_artifacts, StagedHookGuard, StagedHookPoint,
};
use std::path::PathBuf;
use std::sync::Arc;

/// Runs `scan`, deleting `victim` at the moment the scan has listed it but not
/// yet stat'ed it, and returns the scan's result.
fn with_entry_removed_after_listing<T: Send + 'static>(
    victim: &Path,
    scan: impl FnOnce() -> T + Send + 'static,
) -> T {
    let hook = StagedHookGuard::arm(victim, StagedHookPoint::ScanEntryListed);
    let worker = std::thread::spawn(scan);
    hook.wait_until_reached_for(Duration::from_secs(5));
    if victim.is_dir() {
        std::fs::remove_dir_all(victim).unwrap();
    } else {
        std::fs::remove_file(victim).unwrap();
    }
    hook.resume();
    crate::core::wait::join_for(worker, Duration::from_secs(5))
        .unwrap()
        .unwrap()
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
        let result = add_tree(
            &tree,
            &mut artifact,
            &mut HashSet::new(),
            crate::core::wait::PROBE,
        );
        (result, artifact)
    });

    result.expect("a vanished file must not fail the accounting walk");
    assert!(
        artifact.allocated_bytes > 0,
        "the surviving file still counts"
    );
}

/// The production shape, without an injected pause: real publications racing
/// real scans over the live store. Pair each scan with one publication: a
/// producer that runs until scans finish can continually extend the trash walk.
#[test]
fn scans_survive_concurrent_staged_publication() {
    let root = tempfile::tempdir().unwrap();
    let artifact_dir = root.path().to_path_buf();
    let (publish, requests) = std::sync::mpsc::channel();
    let (published, completions) = std::sync::mpsc::channel();
    persist_staged_artifact_payloads(
        &artifact_dir,
        &format!("{:064x}", 0),
        &[Arc::new(0_u32.to_le_bytes().to_vec())],
    )
    .unwrap();
    let publisher = {
        let artifact_dir = artifact_dir.clone();
        std::thread::spawn(move || {
            for round in 0_u32..500 {
                if crate::core::wait::recv_for(&requests, crate::core::wait::PROBE).is_err() {
                    break;
                }
                // One payload per key: a key republished with different bytes
                // fails closed by design, which is not what this exercises.
                let key_index = round % 64;
                let key = format!("{key_index:064x}");
                let payload = Arc::new(key_index.to_le_bytes().to_vec());
                persist_staged_artifact_payloads(&artifact_dir, &key, &[payload]).unwrap();
                published.send(()).unwrap();
            }
        })
    };
    let mut outcome = Ok(());
    for _ in 0..500 {
        publish.send(()).unwrap();
        outcome = scan_artifacts(&artifact_dir).map(|_| ());
        crate::core::wait::recv_for(&completions, crate::core::wait::PROBE).unwrap();
        if outcome.is_err() {
            break;
        }
    }
    drop(publish);
    crate::core::wait::join_for(publisher, crate::core::wait::PROBE)
        .unwrap()
        .unwrap();
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

fn denied() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

fn denied_is_transient(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied
}

#[test]
fn denied_entry_counts_as_vanished_only_once_really_gone() {
    use crate::daemon::server::persist::listed_entry_with;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("deleting");
    std::fs::write(&path, b"x").unwrap();
    let mut attempts = 0;
    let result = listed_entry_with(
        &path,
        |entry| {
            attempts += 1;
            if attempts == 1 {
                std::fs::remove_file(entry)?;
                return Err(denied());
            }
            std::fs::symlink_metadata(entry)
        },
        denied_is_transient,
    );
    assert!(result.unwrap().is_none());
    assert_eq!(attempts, 2);
}

#[test]
fn denied_entry_that_recovers_is_still_scanned() {
    use crate::daemon::server::persist::listed_entry_with;
    let root = tempfile::tempdir().unwrap();
    let mut attempts = 0;
    let result = listed_entry_with(
        root.path(),
        |entry| {
            attempts += 1;
            if attempts == 1 {
                return Err(denied());
            }
            std::fs::symlink_metadata(entry)
        },
        denied_is_transient,
    );
    assert!(result.unwrap().is_some());
}

#[test]
fn retry_never_applies_to_errors_outside_the_classifier() {
    use crate::daemon::server::persist::listed_entry_with;
    let mut attempts = 0;
    let error = listed_entry_with(
        Path::new("unused"),
        |_| -> io::Result<()> {
            attempts += 1;
            Err(denied())
        },
        |_| false,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(attempts, 1);
}

/// Owner-unreadable directory, restored on drop so the tempdir can be removed.
#[cfg(unix)]
struct UnreadableDir<'a>(&'a Path);

#[cfg(unix)]
impl<'a> UnreadableDir<'a> {
    /// `None` when this user ignores the mode bits (root), which proves nothing.
    fn new(path: &'a Path) -> Option<Self> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let guard = Self(path);
        std::fs::read_dir(path).is_err().then_some(guard)
    }
}

#[cfg(unix)]
impl Drop for UnreadableDir<'_> {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

/// A directory that stays denied exhausts the retries and fails the walk: it
/// is present, so reporting it as vanished would hide a partial tree (#2092).
#[cfg(unix)]
#[test]
fn persistently_unreadable_directory_is_not_treated_as_vanished() {
    use crate::daemon::server::persist::listed_entry_with;
    let root = tempfile::tempdir().unwrap();
    let tree = root.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    let Some(_unreadable) = UnreadableDir::new(&tree) else {
        return;
    };

    let mut attempts = 0;
    let error = listed_entry_with(
        &tree,
        |entry| {
            attempts += 1;
            std::fs::read_dir(entry)
        },
        denied_is_transient,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(attempts, 5, "the initial attempt plus four retries");
    assert!(error.to_string().contains("tree"), "{error}");

    let mut artifact = artifact("key", 0, SystemTime::now(), Duration::ZERO);
    let error = add_tree(
        &tree,
        &mut artifact,
        &mut HashSet::new(),
        crate::core::wait::PROBE,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

/// The production classifier: Windows codes 5 and 32 that never clear must
/// surface after the ladder, not read as vanished. These errors are injected;
/// neither a deny ACL nor a held delete-on-close file produced a denied walk
/// or stat on the Windows runners, so real-state coverage is the Unix test.
#[cfg(windows)]
#[test]
fn persistent_windows_share_errors_are_not_treated_as_vanished() {
    use crate::daemon::server::persist::listed_entry;
    for code in [5, 32] {
        let mut attempts = 0;
        let error = listed_entry(Path::new("denied-entry"), |_| -> io::Result<()> {
            attempts += 1;
            Err(io::Error::from_raw_os_error(code))
        })
        .unwrap_err();
        assert_eq!(attempts, 5, "the initial attempt plus four retries");
        assert!(error.to_string().contains("denied-entry"), "{error}");
    }
}
