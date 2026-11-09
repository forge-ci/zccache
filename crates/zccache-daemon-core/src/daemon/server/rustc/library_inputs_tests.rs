#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

const ROOT: &str = "Crate info:\nname umbrella-123\nhash 123 stable_crate_id StableCrateId(7)\nproc_macro false\ntriple x86_64-unknown-linux-gnu\n=External Dependencies=\n1 choice-456 hash abc host_hash None kind Explicit public\n\n";

#[test]
fn metadata_preserves_exact_crate_identities_and_fails_closed() {
    let metadata = library_metadata::parse(ROOT).unwrap();
    assert_eq!(metadata.stem, "umbrella-123");
    assert_eq!(metadata.hash, 0x123);
    assert_eq!(metadata.dependencies[0].stem, "choice-456");
    assert_eq!(metadata.dependencies[0].hash, 0xabc);
    for bad in [
        ROOT.replace("host_hash None", "host_hash Some(123)"),
        ROOT.replace("1 choice", "2 choice"),
        ROOT.replace("choice-456", "../choice"),
        ROOT.replace("=External Dependencies=", "unknown"),
    ] {
        assert!(library_metadata::parse(&bad).is_err());
    }
    assert!(library_metadata::parse("unknown -Zls kind").is_err());
    assert!(
        library_metadata::parse(ROOT.trim_end()).is_err(),
        "truncated lists must not be admitted"
    );
}

#[test]
fn named_candidates_include_metadata_and_codegen_but_not_unrelated_files() {
    let fixture = tempfile::tempdir().unwrap();
    let one: NormalizedPath = fixture.path().join("one").into();
    let two: NormalizedPath = fixture.path().join("two").into();
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    for name in ["libchoice.rlib", "libchoice.rmeta", "libunrelated.rlib"] {
        std::fs::write(one.join(name), b"fixture").unwrap();
    }
    assert_eq!(
        candidates("choice", std::slice::from_ref(&one))
            .unwrap()
            .len(),
        2
    );
    assert!(candidates("missing", std::slice::from_ref(&one)).is_err());
    std::fs::write(two.join("libchoice.rlib"), b"other").unwrap();
    assert!(candidates("choice", &[one, two]).is_err());
}

#[test]
fn perf_library_metadata_warm_lookup_does_not_spawn_rustc() {
    let compiler = crate::test_support::find_rustc().expect("real rustc required");
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("probe.rs");
    let library: NormalizedPath = fixture.path().join("libprobe.rmeta").into();
    std::fs::write(&source, "pub fn value() -> u32 { 17 }").unwrap();
    let mut command = std::process::Command::new(compiler.as_path());
    command
        .args(["--crate-type=rlib", "--crate-name=probe", "--emit=metadata"])
        .arg(&source)
        .arg("-o")
        .arg(library.as_path());
    let output = zccache_test_support::test_child::TestChild::spawn(command)
        .unwrap()
        .finish(std::time::Duration::from_secs(30))
        .unwrap();
    assert_eq!(output.code, Some(0), "{}", output.stderr_text());
    kernal_api::platform::fs::set_file_mtime(
        library.as_path(),
        kernal_api::platform::fs::FileTime::from_unix_time(1_600_000_000, 0),
    )
    .unwrap();
    let files = MetadataCache::new();
    let compiler_hash = crate::hash::hash_bytes(b"1930 real metadata warm lookup");
    let before = library_metadata::probe_count();
    let first =
        library_metadata::inspect(&compiler, compiler_hash, &library, &files, fixture.path())
            .unwrap();
    let directory =
        library_metadata::target_directory(&compiler, compiler_hash, &first.triple, fixture.path())
            .unwrap();
    for _ in 0..20 {
        let again =
            library_metadata::inspect(&compiler, compiler_hash, &library, &files, fixture.path())
                .unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(
            library_metadata::target_directory(
                &compiler,
                compiler_hash,
                &first.triple,
                fixture.path()
            )
            .unwrap(),
            directory
        );
    }
    assert_eq!(
        library_metadata::probe_count(),
        before + 2,
        "warm hits must reuse decoded metadata"
    );
}
