//! Imported PCH provenance and strict compiler regression fixtures.

use super::*;
use std::process::Command;

fn unavailable_compiler(reason: &str, required: bool) {
    assert!(
        !required,
        "ZCCACHE_PCH_TESTS_REQUIRED=1 requires Clang >= 19 for provenance tests: {reason}"
    );
    eprintln!("skipping PCH provenance fixture: {reason}");
}

fn clang_compiler() -> Option<&'static NormalizedPath> {
    static COMPILER: std::sync::OnceLock<Result<NormalizedPath, String>> =
        std::sync::OnceLock::new();
    let result = COMPILER.get_or_init(|| {
        let compiler = crate::test_support::find_clang()
            .ok_or_else(|| "Clang compiler is unavailable".to_owned())?;
        let output = Command::new(&compiler)
            .arg("--version")
            .output()
            .map_err(|error| format!("{}: {error}", compiler.display()))?;
        let version = String::from_utf8_lossy(&output.stdout);
        let major = version
            .split("version ")
            .nth(1)
            .and_then(|version| version.split('.').next())
            .and_then(|major| major.parse::<u32>().ok());
        if !output.status.success() || major.is_none_or(|major| major < 19) {
            return Err(format!(
                "{} does not provide supported Clang >= 19: {}",
                compiler.display(),
                version.trim()
            ));
        }
        Ok(compiler)
    });
    match result {
        Ok(compiler) => Some(compiler),
        Err(reason) => {
            unavailable_compiler(
                reason,
                std::env::var("ZCCACHE_PCH_TESTS_REQUIRED").as_deref() == Ok("1"),
            );
            None
        }
    }
}

#[test]
#[should_panic(expected = "ZCCACHE_PCH_TESTS_REQUIRED=1 requires Clang >= 19")]
fn required_missing_compiler_is_a_failure() {
    unavailable_compiler("Clang compiler is unavailable", true);
}

fn fixture_command(root: &Path) -> Command {
    let compiler = clang_compiler().expect("fixture must establish compiler prerequisites");
    let mut command = Command::new(compiler);
    command.current_dir(root);
    // Windows runners expose an x64 CRT, including ARM runners whose
    // native clang defaults to ARM64. Keep every PCH and its executable
    // on the same target; Windows ARM runs the x64 consumer in emulation.
    #[cfg(windows)]
    command.arg("--target=x86_64-pc-windows-msvc");
    command
}

fn clang(root: &Path, args: &[&str]) {
    let output = fixture_command(root)
        .args(args)
        .args([
            "-Werror=invalid-pch",
            "-fpch-validate-input-files-content",
            "-Wno-unused-command-line-argument",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> Option<tempfile::TempDir> {
    clang_compiler()?;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("base.h"),
        "#pragma once\nconstexpr int value = 1;\nstruct A { int x; };\n",
    )
    .unwrap();
    std::fs::write(
        root.join("top.h"),
        "#pragma once\nconstexpr int top_value = value;\n",
    )
    .unwrap();
    clang(
        root,
        &[
            "-x",
            "c++-header",
            "base.h",
            "-o",
            "base.pch",
            "-verify-pch",
        ],
    );
    clang(
        root,
        &[
            "-x",
            "c++-header",
            "top.h",
            "-include-pch",
            "base.pch",
            "-o",
            "top.pch",
            "-verify-pch",
        ],
    );
    Some(directory)
}

fn prepare_view(
    state: &SharedState,
    root: &Path,
    pch: &Path,
    freeze: bool,
) -> Result<Option<super::super::pch_overlay::PchOverlay>, crate::fingerprint::pch_inputs::PchRefusal>
{
    let sid = state.sessions.create(crate::depgraph::SessionConfig {
        client_pid: std::process::id(),
        working_dir: root.into(),
        log_file: None,
        track_stats: true,
        journal_path: None,
        profile: false,
        private_env: Vec::new(),
        owner_pids: Vec::new(),
    });
    super::super::pch_overlay::prepare_loaded_pch_overlay(
        state,
        &sid,
        &[NormalizedPath::new(pch)],
        root,
        None,
        &[],
        freeze,
    )
}

fn consumer(root: &Path, pch: &str, name: &str, overlay: Option<&Path>) -> std::process::Output {
    let executable = root.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let mut command = fixture_command(root);
    command
        .args(["use.cpp", "-include-pch", pch, "-o"])
        .arg(&executable)
        .args(["-Werror=invalid-pch", "-fpch-validate-input-files-content"]);
    if let Some(overlay) = overlay {
        command.arg("-ivfsoverlay").arg(overlay);
    }
    command.output().unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_same_stat_replacement_cannot_gain_private_header_repair() {
    let Some(directory) = fixture() else { return };
    let root = directory.path();
    let cache = tempfile::tempdir().unwrap();
    let server = crate::daemon::server::tests::bind_isolated_server(cache.path());
    let top = root.join("top.pch");
    let base = root.join("base.pch");
    assert!(
        prepare_view(server.state.as_ref(), root, &top, false)
            .unwrap()
            .is_none(),
        "a bare legacy consumer needs no invented provenance"
    );
    let original = std::fs::read(&base).unwrap();
    let original_time = std::fs::metadata(&base).unwrap().modified().unwrap();
    std::fs::write(
        root.join("base.h"),
        "#pragma once\nconstexpr int value = 2;\nstruct A { int x; };\n",
    )
    .unwrap();
    clang(
        root,
        &[
            "-x",
            "c++-header",
            "base.h",
            "-o",
            "base.pch",
            "-verify-pch",
        ],
    );
    assert_eq!(
        original.len(),
        std::fs::metadata(&base).unwrap().len() as usize
    );
    std::fs::File::options()
        .write(true)
        .open(&base)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_time))
        .unwrap();
    let header = root.join("base.h");
    let touched = std::fs::metadata(&header).unwrap().modified().unwrap()
        + std::time::Duration::from_secs(100);
    std::fs::File::options()
        .write(true)
        .open(&header)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(touched))
        .unwrap();
    std::fs::write(
        root.join("use.cpp"),
        "#include \"base.h\"\nint main(){return value * 10 + top_value;}\n",
    )
    .unwrap();
    let raw = consumer(root, "top.pch", "raw", None);
    assert!(
        !raw.status.success(),
        "strict clang must reject the unrepaired input"
    );
    let view = prepare_view(server.state.as_ref(), root, &top, false);
    if let Ok(Some(view)) = &view {
        let unsafe_result = consumer(root, "top.pch", "unsafe", Some(view.path()));
        assert!(
            unsafe_result.status.success(),
            "{}",
            String::from_utf8_lossy(&unsafe_result.stderr)
        );
        assert_eq!(
            Command::new(root.join(format!("unsafe{}", std::env::consts::EXE_SUFFIX)))
                .status()
                .unwrap()
                .code(),
            Some(21)
        );
    }
    clang(
        root,
        &[
            "-x",
            "c++-header",
            "base.h",
            "-o",
            "base.pch",
            "-verify-pch",
        ],
    );
    clang(
        root,
        &[
            "-x",
            "c++-header",
            "top.h",
            "-include-pch",
            "base.pch",
            "-o",
            "fresh.pch",
            "-verify-pch",
        ],
    );
    let fresh = consumer(root, "fresh.pch", "fresh", None);
    assert!(
        fresh.status.success(),
        "{}",
        String::from_utf8_lossy(&fresh.stderr)
    );
    assert_eq!(
        Command::new(root.join(format!("fresh{}", std::env::consts::EXE_SUFFIX)))
            .status()
            .unwrap()
            .code(),
        Some(22)
    );
    assert!(view.is_err(), "missing parent baseline must refuse private repair instead of accepting mixed AST value 21");
    assert_eq!(
        std::fs::metadata(header).unwrap().modified().unwrap(),
        touched
    );
}

#[tokio::test(flavor = "current_thread")]
async fn producer_freeze_requires_parent_baseline_even_when_all_stats_match() {
    let Some(directory) = fixture() else { return };
    let cache = tempfile::tempdir().unwrap();
    let server = crate::daemon::server::tests::bind_isolated_server(cache.path());
    assert!(
        prepare_view(
            server.state.as_ref(),
            directory.path(),
            &directory.path().join("top.pch"),
            true
        )
        .is_err(),
        "freezing a legacy parent cannot bless its current unsigned imports"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn valid_outer_manifest_cannot_hide_missing_nested_parent_baseline() {
    let Some(directory) = fixture() else { return };
    let root = directory.path();
    std::fs::write(
        root.join("outer.h"),
        "#pragma once\nconstexpr int outer_value = top_value;\n",
    )
    .unwrap();
    clang(
        root,
        &[
            "-x",
            "c++-header",
            "outer.h",
            "-include-pch",
            "top.pch",
            "-o",
            "outer.pch",
            "-verify-pch",
        ],
    );
    let outer = root.join("outer.pch");
    record_pch_imports(&outer, None).unwrap();
    let primary = CachedPayload::Bytes(Arc::new(std::fs::read(&outer).unwrap()));
    let manifest = CachedPayload::Bytes(Arc::new(std::fs::read(sidecar_path(&outer)).unwrap()));
    assert!(
        verify_cached_imports(&primary, Some(&manifest)).is_err(),
        "cached outer PCH must require bound nested provenance recursively"
    );
    let header = root.join("base.h");
    let touched = std::fs::metadata(&header).unwrap().modified().unwrap()
        + std::time::Duration::from_secs(100);
    std::fs::File::options()
        .write(true)
        .open(&header)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(touched))
        .unwrap();
    let cache = tempfile::tempdir().unwrap();
    let server = crate::daemon::server::tests::bind_isolated_server(cache.path());
    assert!(prepare_view(server.state.as_ref(), root, &outer, false).is_err());
    assert!(prepare_view(server.state.as_ref(), root, &outer, true).is_err());
}

#[test]
fn producer_recording_requires_executed_import_snapshot() {
    let Some(directory) = fixture() else { return };
    let root = directory.path();
    let top = root.join("top.pch");
    assert!(
        record_pch_imports_for_output(&top, &top, None).is_err(),
        "physical imports cannot establish which bytes the producer actually loaded"
    );
}

#[test]
fn cached_parent_requires_bound_original_manifest() {
    let Some(directory) = fixture() else { return };
    let root = directory.path();
    let pch = root.join("top.pch");
    record_pch_imports(&pch, None).unwrap();
    let mut bytes = std::fs::read(&pch).unwrap();
    let manifest = CachedPayload::Bytes(Arc::new(std::fs::read(sidecar_path(&pch)).unwrap()));
    let primary = CachedPayload::Bytes(Arc::new(bytes.clone()));
    verify_cached_imports(&primary, Some(&manifest)).unwrap();
    assert!(verify_cached_imports(&primary, None).is_err());
    let corrupt = CachedPayload::Bytes(Arc::new(b"{}".to_vec()));
    assert!(verify_cached_imports(&primary, Some(&corrupt)).is_err());
    bytes.push(0);
    let replaced = CachedPayload::Bytes(Arc::new(bytes));
    assert!(verify_cached_imports(&replaced, Some(&manifest)).is_err());
}

#[test]
fn imported_hashes_freeze_verified_bytes_without_rewinding_original() {
    let Some(directory) = fixture() else { return };
    let root = directory.path();
    let top = root.join("top.pch");
    let base = root.join("base.pch");
    record_pch_imports(&top, None).unwrap();
    let moved =
        std::fs::metadata(&base).unwrap().modified().unwrap() + std::time::Duration::from_secs(100);
    std::fs::File::options()
        .write(true)
        .open(&base)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(moved))
        .unwrap();
    let hashes = verified_pch_import_hashes(&top).unwrap();
    let expected = crate::fingerprint::pch_inputs::content_xxh3(&std::fs::read(&base).unwrap());
    let record = imports(&std::fs::read(&top).unwrap())
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(hashes[&record.path], expected);
    assert_eq!(std::fs::metadata(&base).unwrap().modified().unwrap(), moved);
    std::fs::remove_file(sidecar_path(&top)).unwrap();
    assert!(
        verified_pch_import_hashes(&top).is_err(),
        "external unsigned import drift needs a producer baseline"
    );
}

#[test]
fn recording_uses_frozen_import_despite_same_size_same_mtime_replacement() {
    let Some(directory) = fixture() else { return };
    let root = directory.path();
    let base = root.join("base.pch");
    let top = root.join("top.pch");
    let original = std::fs::read(&base).unwrap();
    let original_time = std::fs::metadata(&base).unwrap().modified().unwrap();
    let snapshot = root.join("base-snapshot.pch");
    std::fs::write(&snapshot, &original).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&snapshot)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_time))
        .unwrap();
    let record = imports(&std::fs::read(&top).unwrap())
        .unwrap()
        .pop()
        .unwrap();
    let overlay = root.join("overlay.json");
    std::fs::write(&overlay, serde_json::to_vec(&serde_json::json!({"version":0,"use-external-names":false,"roots":[{"type":"file","name":record.path,"external-contents":snapshot}]})).unwrap()).unwrap();
    std::fs::write(
        root.join("base.h"),
        "#pragma once\nconstexpr int value = 2;\nstruct A { int x; };\n",
    )
    .unwrap();
    clang(
        root,
        &[
            "-x",
            "c++-header",
            "base.h",
            "-o",
            "base.pch",
            "-verify-pch",
        ],
    );
    std::fs::File::options()
        .write(true)
        .open(&base)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_time))
        .unwrap();
    assert!(record_pch_imports_for_output(&top, &top, None).is_err());
    record_pch_imports(&top, Some(&overlay)).unwrap();
    let manifest: Provenance =
        serde_json::from_slice(&std::fs::read(sidecar_path(&top)).unwrap()).unwrap();
    assert_eq!(
        manifest.imports[0].blake3,
        crate::hash::hash_bytes(&original).to_hex()
    );
    assert!(verify_pch_imports(&top).is_err());
}

#[test]
fn detached_companion_snapshot_survives_logical_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let logical = directory.path().join("top.pch.zccache-imports.json");
    std::fs::write(&logical, b"old producer").unwrap();
    let mut owner = None;
    let private =
        preserve_import_sidecar(&logical, b"old producer", &mut owner, directory.path()).unwrap();
    std::fs::write(&logical, b"next producer").unwrap();
    assert_eq!(std::fs::read(private).unwrap(), b"old producer");
}

#[test]
fn producer_without_imports_clears_obsolete_companion() {
    let Some(directory) = fixture() else { return };
    let root = directory.path();
    let logical = root.join("top.pch");
    record_pch_imports(&logical, None).unwrap();
    assert!(sidecar_path(&logical).exists());
    assert!(
        record_pch_imports_for_output(&root.join("base.pch"), &logical, None)
            .unwrap()
            .is_none()
    );
    assert!(!sidecar_path(&logical).exists());
}

#[test]
fn same_size_rebuilt_import_cannot_inherit_old_parent_provenance() {
    let Some(directory) = fixture() else { return };
    let root = directory.path();
    let original = std::fs::read(root.join("base.pch")).unwrap();
    record_pch_imports(&root.join("top.pch"), None).unwrap();
    std::fs::write(
        root.join("base.h"),
        "#pragma once\nconstexpr int value = 2;\nstruct A { int x; };\n",
    )
    .unwrap();
    clang(
        root,
        &[
            "-x",
            "c++-header",
            "base.h",
            "-o",
            "base.pch",
            "-verify-pch",
        ],
    );
    let replacement = std::fs::read(root.join("base.pch")).unwrap();
    assert_eq!(original.len(), replacement.len());
    assert_ne!(
        crate::hash::hash_bytes(&original),
        crate::hash::hash_bytes(&replacement)
    );
    assert!(
        verify_pch_imports(&root.join("top.pch")).is_err(),
        "same-size replacement must be refused even if clang import timestamps match"
    );
}
