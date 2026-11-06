use super::*;
use kernal_api::platform::fs::DirectoryWalk;

#[test]
fn empty_directories_and_nested_files_walk_cleanly() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("a/empty")).unwrap();
    std::fs::create_dir_all(root.path().join("b/c")).unwrap();
    std::fs::write(root.path().join("b/c/file"), b"x").unwrap();
    let walk = DirectoryWalk::new(root.path().to_path_buf());
    let entries: Vec<_> = walk_reporting_unreadable(walk)
        .collect::<io::Result<_>>()
        .unwrap();
    // Root, a, a/empty, b, b/c, b/c/file.
    assert_eq!(entries.len(), 6);
}

#[test]
fn a_directory_removed_before_its_probe_is_not_a_failure() {
    let root = tempfile::tempdir().unwrap();
    probe_directory(&root.path().join("gone")).unwrap();
}

#[cfg(unix)]
#[test]
fn an_unreadable_directory_is_reported_and_its_siblings_still_walk() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let denied = root.path().join("denied");
    std::fs::create_dir(&denied).unwrap();
    std::fs::write(root.path().join("sibling"), b"x").unwrap();
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o000)).unwrap();
    let bypassed = std::fs::read_dir(&denied).is_ok();
    let walk = DirectoryWalk::new(root.path().to_path_buf());
    let items: Vec<_> = walk_reporting_unreadable(walk).collect();
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o700)).unwrap();
    if bypassed {
        eprintln!("skipping: this user bypasses directory permissions");
        return;
    }
    let errors: Vec<_> = items.iter().filter_map(|i| i.as_ref().err()).collect();
    assert_eq!(errors.len(), 1, "{items:?}");
    assert_eq!(errors[0].kind(), io::ErrorKind::PermissionDenied);
    assert!(errors[0].to_string().contains("denied"));
    assert_eq!(items.len(), 4, "root, denied, its error, sibling");
}
