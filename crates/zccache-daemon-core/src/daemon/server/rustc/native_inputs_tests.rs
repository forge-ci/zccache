#![allow(clippy::expect_used)]

use super::*;

fn parsed(raw: &[&str], cwd: &Path) -> (RustcParsedArgs, Vec<String>) {
    let mut raw: Vec<String> = raw.iter().map(|s| (*s).into()).collect();
    raw.extend([
        "--target=x86_64-unknown-linux-gnu".into(),
        "src/lib.rs".into(),
    ]);
    (crate::depgraph::parse_rustc_args(&raw, cwd), raw)
}

fn archive(root: &Path, directory: &str, name: &str) -> NormalizedPath {
    let directory = root.join(directory);
    std::fs::create_dir_all(&directory).expect("directory");
    let path = directory.join(name);
    std::fs::write(&path, b"archive content").expect("archive");
    path.into()
}

#[test]
fn native_archive_inputs_cover_static_plain_and_all_search_paths() {
    let temp = tempfile::tempdir().expect("fixture");
    let path = archive(temp.path(), "native", "libfoo.a");
    for library in ["static=foo", "foo", "static:+bundle=foo"] {
        for search in ["native=native", "all=native", "native"] {
            let (args, raw) = parsed(
                &["--crate-type=rlib", "-L", search, "-l", library],
                temp.path(),
            );
            let inputs = inputs(&args, &raw, temp.path()).expect("resolved");
            assert_eq!(inputs.len(), 1);
            assert_eq!(inputs[0].path, path);
        }
    }
}

#[test]
fn native_archive_search_order_is_first_directory_not_sorted_path() {
    let temp = tempfile::tempdir().expect("fixture");
    let first = archive(temp.path(), "z", "libfoo.a");
    let second = archive(temp.path(), "a", "libfoo.a");
    for (dirs, expected) in [
        (["-Lnative=z", "-Lall=a"], first),
        (["-Lall=a", "-Lnative=z"], second),
    ] {
        let (args, raw) = parsed(&[dirs[0], dirs[1], "-lstatic=foo"], temp.path());
        assert_eq!(
            inputs(&args, &raw, temp.path()).expect("resolved")[0].path,
            expected
        );
    }
}

#[test]
fn native_archive_resolution_fails_closed_without_eligible_search_path() {
    let temp = tempfile::tempdir().expect("fixture");
    archive(temp.path(), "native", "libfoo.a");
    for search in [
        "dependency=native",
        "crate=native",
        "framework=native",
        "native=missing",
    ] {
        let (args, raw) = parsed(&["-L", search, "-lstatic=foo"], temp.path());
        assert!(inputs(&args, &raw, temp.path()).is_err());
    }
}

#[test]
fn native_archive_msvc_spelling_and_ambiguity_use_compiler_target() {
    let temp = tempfile::tempdir().expect("fixture");
    let path = archive(temp.path(), "native", "foo.lib");
    let (mut args, raw) = parsed(&["-Lnative=native", "-lstatic=foo"], temp.path());
    args.target = Some("x86_64-pc-windows-msvc".into());
    assert_eq!(
        inputs(&args, &raw, temp.path()).expect("resolved")[0].path,
        path
    );
    archive(temp.path(), "native", "libfoo.a");
    assert!(inputs(&args, &raw, temp.path()).is_err());
}

#[test]
fn native_archive_plain_library_rejects_a_competing_dynamic_library() {
    let temp = tempfile::tempdir().expect("fixture");
    archive(temp.path(), "native", "libfoo.a");
    archive(temp.path(), "native", "libfoo.so");
    let (args, raw) = parsed(&["-Lnative=native", "-lfoo"], temp.path());
    assert!(inputs(&args, &raw, temp.path()).is_err());
    let (args, raw) = parsed(&["-Lnative=native", "-lstatic=foo"], temp.path());
    assert!(inputs(&args, &raw, temp.path()).is_ok());
}

#[test]
fn native_archive_failure_does_not_publish_a_partial_extern_set() {
    let temp = tempfile::tempdir().expect("fixture");
    archive(temp.path(), "native", "libfoo.a");
    let (mut args, raw) = parsed(
        &["-Lnative=native", "-lstatic=foo", "-lstatic=missing"],
        temp.path(),
    );
    assert!(!add_inputs(&mut args, &raw, temp.path()));
    assert!(args.externs.is_empty());
    for spec in ["static:+verbatim=foo", "static=foo:renamed", "unknown=foo"] {
        let (args, raw) = parsed(&["-Lnative=native", "-l", spec], temp.path());
        assert!(inputs(&args, &raw, temp.path()).is_err());
    }
}

#[test]
fn native_linked_products_hash_transitive_archives_without_explicit_l_flags() {
    let temp = tempfile::tempdir().expect("fixture");
    let first = archive(temp.path(), "native", "libfoo.a");
    let second = archive(temp.path(), "native", "bar.LIB");
    archive(temp.path(), "native", "not-an-archive.txt");
    for kind in ["bin", "staticlib"] {
        let (args, raw) = parsed(&["--crate-type", kind, "-Lnative=native"], temp.path());
        let paths: Vec<_> = inputs(&args, &raw, temp.path())
            .expect("resolved")
            .into_iter()
            .map(|input| input.path)
            .collect();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&first));
        assert!(paths.contains(&second));
    }
    let (args, raw) = parsed(&["--crate-type=rlib", "-Lnative=native"], temp.path());
    assert!(inputs(&args, &raw, temp.path()).expect("rlib").is_empty());
    let (args, raw) = parsed(&["--crate-type=bin", "-Lnative=missing"], temp.path());
    assert!(inputs(&args, &raw, temp.path()).is_err());
}

#[test]
fn native_archive_unknown_windows_abi_requires_matching_resolutions() {
    let temp = tempfile::tempdir().expect("fixture");
    let common = archive(temp.path(), "common", "libfoo.a");
    let dirs = vec![temp.path().join("common").into()];
    assert_eq!(
        resolve_target_archive("foo", true, &dirs, temp.path(), None, true)
            .expect("both ABIs agree"),
        common
    );
    let msvc = archive(temp.path(), "early", "foo.lib");
    let dirs = vec![
        temp.path().join("early").into(),
        temp.path().join("common").into(),
    ];
    assert!(resolve_target_archive("foo", true, &dirs, temp.path(), None, true).is_err());
    assert_eq!(
        resolve_target_archive(
            "foo",
            true,
            &dirs,
            temp.path(),
            Some("x86_64-pc-windows-msvc"),
            true
        )
        .expect("explicit MSVC"),
        msvc
    );
    assert_eq!(
        resolve_target_archive(
            "foo",
            true,
            &dirs,
            temp.path(),
            Some("x86_64-pc-windows-gnu"),
            true
        )
        .expect("explicit GNU"),
        common
    );
    let dirs = vec![temp.path().join("early").into()];
    assert!(resolve_target_archive("foo", true, &dirs, temp.path(), None, true).is_err());
    assert!(
        resolve_target_archive("foo", true, &dirs, temp.path(), Some("custom.json"), false)
            .is_err()
    );
}
