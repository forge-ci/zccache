#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

fn scan(root: &Path) -> std::io::Result<HashMap<NormalizedPath, FileState>> {
    let config = build_config(root, &[], &["**/*.cpp".to_string()], &[])?;
    scan_snapshot(
        &config,
        DirectoryWalkParallelism::DedicatedPool { threads: 0 },
    )
}

/// Applies `mode` to `dir`, scans, restores it. `None` when this user
/// ignores permissions (root), which cannot produce the failure.
fn scan_with_mode(
    root: &Path,
    dir: &Path,
    mode: u32,
) -> Option<std::io::Result<HashMap<NormalizedPath, FileState>>> {
    let probe = dir.join("watch.cpp");
    fs::set_permissions(dir, fs::Permissions::from_mode(mode)).unwrap();
    let bypassed = fs::read_dir(dir).is_ok() && fs::metadata(&probe).is_ok();
    let result = (!bypassed).then(|| scan(root));
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    result
}

#[test]
fn a_stat_denied_file_fails_the_scan_instead_of_reading_as_deleted() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("watch.cpp"), "a\n").unwrap();
    // Listable but not searchable: the name is readable, its stat is not.
    let Some(result) = scan_with_mode(dir.path(), &sub, 0o400) else {
        return;
    };
    let message = result.expect_err("EACCES is not a deletion").to_string();
    assert!(message.contains("watch.cpp"), "names the path: {message}");
}

#[test]
fn an_unreadable_directory_fails_the_scan_instead_of_reading_as_empty() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("watch.cpp"), "a\n").unwrap();
    let Some(result) = scan_with_mode(dir.path(), &sub, 0o000) else {
        return;
    };
    let message = result.expect_err("a blind directory").to_string();
    assert!(message.contains("sub"), "names the path: {message}");
}
