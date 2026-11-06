#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::os::unix::fs::PermissionsExt;

/// Real mode-000 directory; `None` when the user ignores permissions (root).
fn deny(dir: &Path) -> Option<()> {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(dir).is_ok() {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        eprintln!("skipping: this user bypasses directory permissions");
        return None;
    }
    Some(())
}

fn assert_names_denied_dir(result: Result<Vec<ScannedFile>>, denied: &Path) {
    std::fs::set_permissions(denied, std::fs::Permissions::from_mode(0o700)).unwrap();
    let message = result.expect_err("a partial tree must fail").to_string();
    assert!(
        message.contains(&denied.display().to_string()),
        "error must name the directory: {message}"
    );
}

#[test]
fn walk_files_fails_on_an_unreadable_directory() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let denied = root_path.join("denied");
    std::fs::create_dir(&denied).unwrap();
    std::fs::write(denied.join("hidden.rs"), b"x").unwrap();
    std::fs::write(root_path.join("kept.rs"), b"x").unwrap();
    if deny(&denied).is_none() {
        return;
    }
    assert_names_denied_dir(walk_files(&root_path, &[], &[]), &denied);
}

#[test]
fn walk_files_glob_fails_on_an_unreadable_directory() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let denied = root_path.join("denied");
    std::fs::create_dir(&denied).unwrap();
    std::fs::write(denied.join("hidden.rs"), b"x").unwrap();
    if deny(&denied).is_none() {
        return;
    }
    assert_names_denied_dir(walk_files_glob(&root_path, &[], &[]), &denied);
}

#[test]
fn empty_and_pruned_directories_are_not_failures() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    std::fs::create_dir(root_path.join("empty")).unwrap();
    let pruned = root_path.join("target");
    std::fs::create_dir(&pruned).unwrap();
    if deny(&pruned).is_none() {
        return;
    }
    let result = walk_files(&root_path, &[], &["target"]);
    std::fs::set_permissions(&pruned, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.unwrap().is_empty());
}
