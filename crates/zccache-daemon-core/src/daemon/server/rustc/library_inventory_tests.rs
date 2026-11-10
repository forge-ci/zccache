#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn candidate_names_cover_rust_flavors_without_staticlib_outputs() {
    for name in [
        "libunit.rlib",
        "libunit.RMETA",
        "unit.DLL",
        "libunit.so",
        "libunit.dylib",
        "unit.dll.lib",
        "libunit.DLL.A",
    ] {
        assert!(candidate(std::path::Path::new(name)), "{name}");
    }
    for name in ["libconsumer.a", "consumer.lib", "consumer.o", "consumer.d"] {
        assert!(!candidate(std::path::Path::new(name)), "{name}");
    }
}

fn age(path: &std::path::Path) {
    kernal_api::platform::fs::set_file_mtime(
        path,
        kernal_api::platform::fs::FileTime::from_unix_time(1_600_000_000, 0),
    )
    .unwrap();
}

#[test]
fn inventory_memo_rechecks_content_and_directory_membership() {
    let fixture = tempfile::tempdir().unwrap();
    let dir: NormalizedPath = fixture.path().join("deps").into();
    std::fs::create_dir(&dir).unwrap();
    let library = dir.join("libchoice.rmeta");
    std::fs::write(&library, b"first").unwrap();
    age(&library);
    age(&dir);
    let first = directory(&dir).unwrap().unwrap();
    let has_identity = MetadataCache::stat_file(&dir).unwrap().identity.change_ns != 0;
    if has_identity {
        assert!(Arc::ptr_eq(&first, &directory(&dir).unwrap().unwrap()));
    }

    let compiler = crate::test_support::find_rustc().expect("real rustc required");
    let compiler_hash = crate::hash::hash_bytes(b"2174 inventory mutation");
    let files = MetadataCache::new();
    let before = inputs(
        std::slice::from_ref(&dir),
        &compiler,
        compiler_hash,
        &files,
        fixture.path(),
    )
    .unwrap()
    .unwrap();
    std::fs::write(&library, b"other").unwrap();
    kernal_api::platform::fs::set_file_mtime(
        &library,
        kernal_api::platform::fs::FileTime::from_unix_time(1_600_000_002, 0),
    )
    .unwrap();
    // Rewriting a member leaves directory membership unchanged. Its bytes must
    // nevertheless be verified rather than trusting the membership memo.
    if has_identity {
        assert!(Arc::ptr_eq(&first, &directory(&dir).unwrap().unwrap()));
    }
    let after = inputs(
        std::slice::from_ref(&dir),
        &compiler,
        compiler_hash,
        &files,
        fixture.path(),
    )
    .unwrap()
    .unwrap();
    assert_ne!(before[0].name, after[0].name);

    let second = dir.join("libearlier.rmeta");
    std::fs::write(&second, b"new candidate").unwrap();
    age(&second);
    let expanded = directory(&dir).unwrap().unwrap();
    assert_eq!(expanded.len(), 2);
    assert!(!Arc::ptr_eq(&first, &expanded));
    std::fs::rename(&library, fixture.path().join("removed.rmeta")).unwrap();
    let reduced = directory(&dir).unwrap().unwrap();
    assert_eq!(reduced.len(), 1);
    assert_eq!(reduced[0], second);
}

#[test]
fn inventory_does_not_memoize_racy_membership_or_unbounded_directories() {
    let fixture = tempfile::tempdir().unwrap();
    let dir: NormalizedPath = fixture.path().into();
    kernal_api::platform::fs::set_file_mtime(
        &dir,
        kernal_api::platform::fs::FileTime::from_system_time(
            SystemTime::now() + Duration::from_secs(60),
        ),
    )
    .unwrap();
    let first = directory(&dir).unwrap().unwrap();
    let second = directory(&dir).unwrap().unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    for index in 0..=MAX_FILES {
        std::fs::write(dir.join(format!("lib{index}.rmeta")), b"metadata").unwrap();
    }
    assert!(directory(&dir).unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn inventory_rechecks_symlink_candidate_kind_without_parent_directory_change() {
    let fixture = tempfile::tempdir().unwrap();
    let dir: NormalizedPath = fixture.path().join("deps").into();
    let target = fixture.path().join("target");
    std::fs::create_dir(&dir).unwrap();
    std::fs::create_dir(&target).unwrap();
    let link = dir.join("libchoice.rmeta");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    age(&dir);
    let first = directory(&dir).unwrap().unwrap();
    assert_eq!(first.as_slice(), std::slice::from_ref(&link));
    let compiler = crate::test_support::find_rustc().expect("real rustc required");
    let compiler_hash = crate::hash::hash_bytes(b"2174 inventory symlink kind");
    let files = MetadataCache::new();
    let before = inputs(
        std::slice::from_ref(&dir),
        &compiler,
        compiler_hash,
        &files,
        fixture.path(),
    )
    .unwrap()
    .unwrap();
    assert!(!before.iter().any(|input| input.path == link));
    std::fs::rename(&target, fixture.path().join("old-directory")).unwrap();
    std::fs::write(&target, b"now a candidate file").unwrap();
    assert!(Arc::ptr_eq(&first, &directory(&dir).unwrap().unwrap()));
    let after = inputs(
        std::slice::from_ref(&dir),
        &compiler,
        compiler_hash,
        &files,
        fixture.path(),
    )
    .unwrap()
    .unwrap();
    assert!(after.iter().any(|input| input.path == link));
}
