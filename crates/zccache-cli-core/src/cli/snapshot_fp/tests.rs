use super::*;
use std::io::Write;

fn write(path: &Path, content: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(content.as_bytes()).unwrap();
}

/// Synthetic layout for a single lib crate "foo" at `workspace/crates/foo/`.
fn make_synthetic_lib(root: &Path, source_content: &str) -> (PathBuf, PathBuf) {
    let workspace = root.join("workspace");
    let target = workspace.join("target");
    let fp_dir = target.join("debug/.fingerprint/foo-abcdef0123456789");
    let deps_dir = target.join("debug/deps");
    std::fs::create_dir_all(&fp_dir).unwrap();
    std::fs::create_dir_all(&deps_dir).unwrap();
    let src = workspace.join("crates/foo/src/lib.rs");
    write(&src, source_content);
    write(
        &workspace.join("crates/foo/Cargo.toml"),
        "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
    );
    write(&fp_dir.join("dep-lib-foo"), "anchor");
    let canon_src = src.canonicalize().unwrap();
    let d_content = format!(
        "{}: {}\n",
        deps_dir.join("libfoo-abcdef0123456789.rmeta").display(),
        canon_src.display()
    );
    write(&deps_dir.join("foo-abcdef0123456789.d"), &d_content);
    (workspace, target)
}

#[test]
fn record_and_validate_unchanged_lib_is_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, target) = make_synthetic_lib(tmp.path(), "fn main() {}\n");
    let manifest = target.join(MANIFEST_FILENAME);
    let stats = record(&target, &workspace, &manifest, "debug").unwrap();
    assert_eq!(stats.crates_recorded, 1);
    assert_eq!(stats.sources_hashed, 1);

    let dep_path = target.join("debug/.fingerprint/foo-abcdef0123456789/dep-lib-foo");
    let before = std::fs::metadata(&dep_path).unwrap().modified().unwrap();
    let stats = validate(&target, &workspace, &manifest, "debug", 60).unwrap();
    assert_eq!(stats.crates_clean, 1);
    assert_eq!(stats.crates_dirty(), 0);
    let after = std::fs::metadata(&dep_path).unwrap().modified().unwrap();
    assert!(after > before);
}

#[test]
fn validate_dirty_lib_is_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, target) = make_synthetic_lib(tmp.path(), "fn main() {}\n");
    let manifest = target.join(MANIFEST_FILENAME);
    record(&target, &workspace, &manifest, "debug").unwrap();

    let dep_path = target.join("debug/.fingerprint/foo-abcdef0123456789/dep-lib-foo");
    let before = std::fs::metadata(&dep_path).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(
        workspace.join("crates/foo/src/lib.rs"),
        "fn main() { println!(\"hi\"); }\n",
    )
    .unwrap();
    let stats = validate(&target, &workspace, &manifest, "debug", 60).unwrap();
    assert_eq!(stats.crates_clean, 0);
    assert_eq!(stats.crates_dirty(), 1);
    let after = std::fs::metadata(&dep_path).unwrap().modified().unwrap();
    assert_eq!(after, before);
}

/// Synthetic build-script COMPILATION fingerprint: dep file inside
/// the fingerprint dir, .d file under `target/<profile>/build/<dir>/`.
#[test]
fn record_handles_build_script_compilation() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let target = workspace.join("target");
    let fp_dir = target.join("debug/.fingerprint/bar-fedcba9876543210");
    let build_dir = target.join("debug/build/bar-fedcba9876543210");
    std::fs::create_dir_all(&fp_dir).unwrap();
    std::fs::create_dir_all(&build_dir).unwrap();
    let src = workspace.join("crates/bar/build.rs");
    write(
        &src,
        "fn main() { println!(\"cargo:rerun-if-changed=build.rs\"); }\n",
    );
    write(
        &workspace.join("crates/bar/Cargo.toml"),
        "[package]\nname = \"bar\"\nversion = \"0.1.0\"\n",
    );
    write(
        &fp_dir.join("dep-build-script-build-script-build"),
        "anchor",
    );
    let canon_src = src.canonicalize().unwrap();
    let d_content = format!(
        "{}: {}\n",
        build_dir
            .join("build_script_build-fedcba9876543210")
            .display(),
        canon_src.display()
    );
    write(
        &build_dir.join("build_script_build-fedcba9876543210.d"),
        &d_content,
    );

    let manifest = target.join(MANIFEST_FILENAME);
    let stats = record(&target, &workspace, &manifest, "debug").unwrap();
    assert_eq!(stats.crates_recorded, 1);
    assert_eq!(stats.sources_hashed, 1);

    let m: Manifest = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(m.crates[0].fingerprint_dir, "bar-fedcba9876543210");
    assert!(m.crates[0].stamp_targets[0]
        .contains("debug/.fingerprint/bar-fedcba9876543210/dep-build-script-build-script-build"));

    let stats = validate(&target, &workspace, &manifest, "debug", 60).unwrap();
    assert_eq!(stats.crates_clean, 1);
}

/// Synthetic run-build-script (RerunIfChanged) fingerprint: json
/// describes paths relative to the crate manifest dir + an output
/// file under `build/<dir>/`.
#[test]
fn record_handles_run_build_script() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let target = workspace.join("target");
    let fp_dir = target.join("debug/.fingerprint/baz-0123456789abcdef");
    let build_dir = target.join("debug/build/baz-0123456789abcdef");
    std::fs::create_dir_all(&fp_dir).unwrap();
    std::fs::create_dir_all(&build_dir).unwrap();
    write(&workspace.join("crates/baz/build.rs"), "fn main() {}\n");
    write(
        &workspace.join("crates/baz/Cargo.toml"),
        "[package]\nname = \"baz\"\nversion = \"0.1.0\"\n",
    );
    let output_file = build_dir.join("output");
    write(&output_file, "build-output");

    let json = serde_json::json!({
        "local": [
            {
                "RerunIfChanged": {
                    "output": "debug/build/baz-0123456789abcdef/output",
                    "paths": ["build.rs"],
                }
            }
        ]
    });
    write(
        &fp_dir.join("run-build-script-build-script-build.json"),
        &serde_json::to_string(&json).unwrap(),
    );

    let manifest = target.join(MANIFEST_FILENAME);
    let stats = record(&target, &workspace, &manifest, "debug").unwrap();
    assert_eq!(stats.crates_recorded, 1);
    assert_eq!(stats.sources_hashed, 1);

    let m: Manifest = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    assert!(m.crates[0].stamp_targets[0].contains("debug/build/baz-0123456789abcdef/output"));

    let before = std::fs::metadata(&output_file).unwrap().modified().unwrap();
    let stats = validate(&target, &workspace, &manifest, "debug", 60).unwrap();
    assert_eq!(stats.crates_clean, 1);
    let after = std::fs::metadata(&output_file).unwrap().modified().unwrap();
    assert!(after > before, "output file mtime should be bumped");
}

#[test]
fn validate_missing_manifest_is_silent_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let stats = validate(
        tmp.path(),
        tmp.path(),
        &tmp.path().join(".missing"),
        "debug",
        60,
    )
    .unwrap();
    assert_eq!(stats.crates_total, 0);
}

#[test]
fn parse_d_file_handles_unix_and_windows_paths() {
    let content = "/tmp/out: /a/b.rs /a/c.rs\nC:\\out: C:\\a\\b.rs C:\\a\\c.rs\n";
    let parsed = parse_d_file(content);
    assert!(parsed.iter().any(|p| p == Path::new("/a/b.rs")));
    assert!(parsed.iter().any(|p| p == Path::new("/a/c.rs")));
    assert!(parsed.iter().any(|p| p == Path::new("C:\\a\\b.rs")));
    assert!(parsed.iter().any(|p| p == Path::new("C:\\a\\c.rs")));
}

#[test]
fn parse_d_file_handles_escaped_spaces() {
    let content = "/out: /a\\ path/foo.rs\n";
    let parsed = parse_d_file(content);
    assert!(parsed.iter().any(|p| p == Path::new("/a path/foo.rs")));
}

#[test]
fn strip_hash_suffix_classifies_correctly() {
    assert_eq!(strip_hash_suffix("foo-0123456789abcdef"), Some("foo"));
    assert_eq!(
        strip_hash_suffix("zccache-cli-0123456789abcdef"),
        Some("zccache-cli")
    );
    assert_eq!(strip_hash_suffix("foo-bar"), None);
    assert_eq!(strip_hash_suffix("noseparator"), None);
    // Too short
    assert_eq!(strip_hash_suffix("foo-abc"), None);
    // Non-hex
    assert_eq!(strip_hash_suffix("foo-zzzzzzzzzzzzzzzz"), None);
}

#[test]
fn parse_package_name_skips_workspace_inherit() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("Cargo.toml");
    write(
        &p,
        "[package]\nname.workspace = true\nversion.workspace = true\n",
    );
    assert_eq!(parse_package_name(&p), None);
}

#[test]
fn parse_package_name_reads_explicit_name() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("Cargo.toml");
    write(&p, "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\n");
    assert_eq!(parse_package_name(&p), Some("my-crate".to_string()));
}

/// Add one cargo unit to a fixture workspace, in real cargo's layout.
fn add_unit(workspace: &Path, pkg: &str, hash: &str, dep_file: &str, d_stem: &str) {
    let debug = workspace.join("target/debug");
    let fp_dir = debug.join(format!(".fingerprint/{pkg}-{hash}"));
    write(&fp_dir.join(dep_file), "anchor");
    let src_dir = workspace.join(pkg).join("src");
    write(&src_dir.join("lib.rs"), "mod nested;\n");
    write(&src_dir.join("nested.rs"), "\n");
    let src_dir = src_dir.canonicalize().unwrap();
    let d_content = format!(
        "out: {} {}\n",
        src_dir.join("lib.rs").display(),
        src_dir.join("nested.rs").display()
    );
    write(&debug.join(format!("deps/{d_stem}-{hash}.d")), &d_content);
}

/// Record, check stamp targets and sources exist, key by fingerprint dir.
fn record_units(workspace: &Path) -> BTreeMap<String, CrateEntry> {
    let target = workspace.join("target");
    let manifest = target.join(MANIFEST_FILENAME);
    record(&target, workspace, &manifest, "debug").unwrap();
    let m: Manifest = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    for entry in &m.crates {
        assert!(target.join(&entry.stamp_targets[0]).is_file());
        assert!(entry.sources.keys().all(|s| workspace.join(s).is_file()));
    }
    m.crates
        .into_iter()
        .map(|e| (e.fingerprint_dir.clone(), e))
        .collect()
}

const HASH_A: &str = "0123456789abcdef";
const HASH_B: &str = "fedcba9876543210";

#[test]
fn record_covers_hyphenated_package_lib_and_unit_test() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    add_unit(ws, "hy-pkg", HASH_A, "dep-lib-hy_pkg", "hy_pkg");
    add_unit(ws, "hy-pkg", HASH_B, "dep-test-lib-hy_pkg", "hy_pkg");

    let units = record_units(ws);
    assert_eq!(units.len(), 2);
    for (hash, dep_file) in [(HASH_A, "dep-lib-hy_pkg"), (HASH_B, "dep-test-lib-hy_pkg")] {
        let dir = format!("hy-pkg-{hash}");
        let entry = &units[&dir];
        assert!(entry.sources.contains_key("hy-pkg/src/nested.rs"));
        assert_eq!(
            entry.stamp_targets,
            [format!("debug/.fingerprint/{dir}/{dep_file}")]
        );
    }
}

#[test]
fn record_preserves_underscore_package_name() {
    let tmp = tempfile::tempdir().unwrap();
    add_unit(tmp.path(), "us_pkg", HASH_A, "dep-lib-us_pkg", "us_pkg");
    let units = record_units(tmp.path());
    assert!(units[&format!("us_pkg-{HASH_A}")]
        .sources
        .contains_key("us_pkg/src/nested.rs"));
}

#[test]
fn record_resolves_explicit_target_names() {
    let cases = [
        ("dep-lib-core_name", "core_name"),
        ("dep-test-lib-core_name", "core_name"),
        ("dep-bin-my-tool", "my_tool"),
        ("dep-test-bin-my-tool", "my_tool"),
        ("dep-test-integration-test-it-x", "it_x"),
        ("dep-example-ex-y", "ex_y"),
        ("dep-bench-be-z", "be_z"),
    ];
    for (dep_file, d_stem) in cases {
        let tmp = tempfile::tempdir().unwrap();
        add_unit(tmp.path(), "my-hy-pkg", HASH_A, dep_file, d_stem);
        let units = record_units(tmp.path());
        assert_eq!(units.len(), 1, "{dep_file} was not resolved");
    }
}

#[test]
fn record_reports_unresolved_dep_info() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path();
    add_unit(ws, "hy-pkg", HASH_A, "dep-lib-hy_pkg", "wrong_name");
    let stats = record(&ws.join("target"), ws, &ws.join("m.json"), "debug").unwrap();
    assert_eq!(stats.crates_recorded, 0);
    assert_eq!(
        stats.unresolved_dep_info,
        [format!("hy-pkg-{HASH_A}/dep-lib-hy_pkg")]
    );
}

/// #2047: a walk error means the crate inventory is incomplete, so `record`
/// must fail instead of writing a manifest over a partial tree.
#[test]
fn record_fails_when_workspace_walk_reports_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("target");
    std::fs::create_dir_all(target.join("debug/.fingerprint")).unwrap();
    let manifest = target.join(MANIFEST_FILENAME);

    let result = record(&target, &tmp.path().join("missing"), &manifest, "debug");

    assert!(result.is_err(), "partial walk produced {result:?}");
    assert!(!manifest.exists(), "no manifest may be written");
}

/// #2092: rustc writes workspace members' `.d` paths relative to the workspace
/// root. This test process runs from the crate dir, never from `workspace`.
#[test]
fn record_resolves_relative_dep_info_paths_against_the_workspace_root() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, target) = make_synthetic_lib(tmp.path(), "fn main() {}\n");
    write(
        &target.join("debug/deps/foo-abcdef0123456789.d"),
        "foo.rmeta: crates/foo/src/lib.rs\n",
    );
    let stats = record(
        &target,
        &workspace,
        &target.join(MANIFEST_FILENAME),
        "debug",
    )
    .unwrap();
    assert_eq!(stats.crates_recorded, 1);
    assert_eq!(stats.sources_hashed, 1);
}

#[test]
fn record_skips_a_dep_info_source_that_no_longer_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, target) = make_synthetic_lib(tmp.path(), "fn main() {}\n");
    write(
        &target.join("debug/deps/foo-abcdef0123456789.d"),
        "foo.rmeta: crates/foo/src/lib.rs crates/foo/src/gone.rs\n",
    );
    let stats = record(
        &target,
        &workspace,
        &target.join(MANIFEST_FILENAME),
        "debug",
    )
    .unwrap();
    assert_eq!(stats.sources_hashed, 1);
}

/// A source that exists but cannot be read must not shrink the manifest.
#[cfg(unix)]
#[test]
fn record_fails_naming_an_unreadable_dep_info_source() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, target) = make_synthetic_lib(tmp.path(), "fn main() {}\n");
    let source = workspace.join("crates/foo/src/lib.rs");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&source).is_ok() {
        eprintln!("skipping: this user bypasses file permissions");
        return;
    }
    let result = record(
        &target,
        &workspace,
        &target.join(MANIFEST_FILENAME),
        "debug",
    );
    let message = result.unwrap_err().to_string();
    assert!(message.contains("lib.rs"), "must name the path: {message}");
}

/// A package hidden in an unreadable directory would silently lose its build
/// script coverage, so manifest discovery must fail instead.
#[cfg(unix)]
#[test]
fn record_fails_naming_an_unreadable_workspace_directory() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, target) = make_synthetic_lib(tmp.path(), "fn main() {}\n");
    let denied = workspace.join("crates/denied");
    std::fs::create_dir(&denied).unwrap();
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&denied).is_ok() {
        eprintln!("skipping: this user bypasses directory permissions");
        return;
    }
    let result = record(
        &target,
        &workspace,
        &target.join(MANIFEST_FILENAME),
        "debug",
    );
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o700)).unwrap();
    let message = result.unwrap_err().to_string();
    assert!(message.contains("denied"), "must name the path: {message}");
}
