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
    std::fs::write(&path, b"!<arch>\narchive content").expect("archive");
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
fn metadata_only_does_not_scan_unrelated_files_in_native_search_directories() {
    let temp = tempfile::tempdir().expect("fixture");
    let explicit = archive(temp.path(), "native", "libprobe.a");
    std::fs::write(temp.path().join("native/flag_check.c"), b"int main() {}\n")
        .expect("build-script output");
    let metadata = [
        "-Lnative=native",
        "-lstatic=probe",
        "--emit=dep-info,metadata",
    ];
    let (args, raw) = parsed(&metadata, temp.path());
    let resolved = inputs(&args, &raw, temp.path()).expect("resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].path, explicit);

    let link = [metadata[0], metadata[1], "--emit=dep-info,metadata,link"];
    let (args, raw) = parsed(&link, temp.path());
    assert!(inputs(&args, &raw, temp.path()).is_err());
}

#[test]
fn native_products_hash_libraries_without_explicit_l_flags() {
    let temp = tempfile::tempdir().expect("fixture");
    let first = archive(temp.path(), "native", "libfoo.a");
    let second = archive(temp.path(), "native", "bar.LIB");
    let verbatim = archive(temp.path(), "native", "choice.custom");
    for kind in ["bin", "staticlib", "rlib", "lib", "proc-macro"] {
        let (args, raw) = parsed(&["--crate-type", kind, "-Lnative=native"], temp.path());
        let paths: Vec<_> = inputs(&args, &raw, temp.path())
            .expect("resolved")
            .into_iter()
            .map(|input| input.path)
            .collect();
        assert_eq!(paths.len(), 3);
        assert!(paths.contains(&first));
        assert!(paths.contains(&second));
        assert!(paths.contains(&verbatim));
    }
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

#[test]
fn native_search_refuses_indirect_and_unknown_formats() {
    for (name, bytes) in [
        ("libchoice.a", &b"!<thin>\nmember.o"[..]),
        ("libchoice.so", &b"INPUT(/other/libchoice.so)"[..]),
        ("choice.custom", &b"INPUT(/other/libchoice.so)"[..]),
        ("helper.o", &b"\x7fELFobject"[..]),
    ] {
        let temp = tempfile::tempdir().expect("fixture");
        let path = archive(temp.path(), "native", name);
        std::fs::write(path, bytes).expect("unsupported format");
        let (args, raw) = parsed(&["--crate-type=rlib", "-Lnative=native"], temp.path());
        assert!(inputs(&args, &raw, temp.path()).is_err(), "{name}");
    }
}

/// A minimal relocatable ELF header: `require_self_contained_native` accepts
/// `e_type == ET_REL` objects as owning their bytes.
fn relocatable_object(root: &Path, name: &str) -> NormalizedPath {
    let path = root.join(name);
    let mut header = vec![0u8; 64];
    header[..4].copy_from_slice(b"\x7fELF");
    header[4] = 2;
    header[5] = 1;
    header[16] = 1;
    std::fs::write(&path, header).expect("object");
    path.into()
}

#[test]
fn link_arg_object_and_archive_operands_are_content_inputs() {
    // #2212: `cargo:rustc-link-arg*=$OUT_DIR/foo.o` keeps a stable path, so
    // only the file's bytes can tell two links apart.
    let temp = tempfile::tempdir().expect("fixture");
    let object = relocatable_object(temp.path(), "foo.o");
    let archive = archive(temp.path(), "out", "libbar.a");
    let absolute = format!("-Clink-arg={}", object.as_path().display());
    for flags in [
        vec![absolute.as_str()],
        vec!["-Clink-arg=foo.o"],
        vec!["-C", "link-args=-Wl,-O1 foo.o"],
        vec!["-Clink-arg=-Wl,--whole-archive,foo.o,--no-whole-archive"],
        vec!["-Clink-arg=-Wl,--just-symbols=foo.o"],
    ] {
        let mut raw = vec!["--crate-type=bin"];
        raw.extend(flags.iter().copied());
        let (args, raw) = parsed(&raw, temp.path());
        let paths: Vec<_> = inputs(&args, &raw, temp.path())
            .expect("resolved")
            .into_iter()
            .map(|input| input.path)
            .collect();
        assert_eq!(paths, vec![object.clone()], "{flags:?}");
    }
    let (args, raw) = parsed(
        &[
            "--crate-type=bin",
            "-Clink-arg=out/libbar.a",
            "-Clink-arg=foo.o",
        ],
        temp.path(),
    );
    let paths: Vec<_> = inputs(&args, &raw, temp.path())
        .expect("resolved")
        .into_iter()
        .map(|input| input.path)
        .collect();
    assert_eq!(paths, vec![archive, object]);
}

#[test]
fn link_arg_flags_without_file_operands_add_no_inputs() {
    let temp = tempfile::tempdir().expect("fixture");
    std::fs::create_dir(temp.path().join("dir")).expect("directory");
    let (args, raw) = parsed(
        &[
            "--crate-type=bin",
            "-Clink-arg=-Wl,-O1",
            "-Clink-arg=-Wl,-rpath,dir",
            "-Clink-arg=missing.o",
            "-Clink-args=-nostartfiles -static",
        ],
        temp.path(),
    );
    assert!(inputs(&args, &raw, temp.path())
        .expect("resolved")
        .is_empty());
}

#[test]
fn link_arg_indirect_or_unknown_files_refuse_caching() {
    for (flag, name, bytes) in [
        (
            "-Clink-arg=-Wl,--version-script=f.map",
            "f.map",
            &b"{ local: *; };"[..],
        ),
        ("-Clink-arg=f.ld", "f.ld", &b"INPUT(/other/libx.a)"[..]),
        (
            "-Clink-arg=libthin.a",
            "libthin.a",
            &b"!<thin>\nmember.o"[..],
        ),
        ("-Clink-arg=@args.rsp", "args.rsp", &b"foo.o"[..]),
    ] {
        let temp = tempfile::tempdir().expect("fixture");
        std::fs::write(temp.path().join(name), bytes).expect("input");
        let (mut args, raw) = parsed(&["--crate-type=bin", flag], temp.path());
        assert!(inputs(&args, &raw, temp.path()).is_err(), "{flag}");
        assert!(!add_inputs(&mut args, &raw, temp.path()), "{flag}");
        assert!(args.externs.is_empty(), "{flag}");
    }
}
