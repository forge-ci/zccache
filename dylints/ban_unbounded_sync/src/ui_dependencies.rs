//! Get real-core UI link inputs without parsing Cargo's wrapped compiler argv.

pub(super) fn core_flags() -> Vec<String> {
    let toolchain = std::env::var("RUSTUP_TOOLCHAIN").expect("UI toolchain must be explicit");
    let output = std::process::Command::new("soldr")
        // Match ci.yml's DYLINT_CARGO_SHIM: this is a new managed toolchain
        // invocation, not a recursive front-door dispatch. Keep the compiler
        // wrapper and explicit nightly toolchain intact.
        .env_remove("IN_SOLDR_PID")
        // A private target dir: the shared one also holds the Dylint
        // library build and its own copies of these crates (E0460/E0463).
        .env(
            "CARGO_TARGET_DIR",
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ui-dependencies"),
        )
        .args([
            "rustup",
            "run",
            &toolchain,
            "cargo",
            "build",
            "--examples",
            "--message-format=json",
        ])
        .output()
        .expect("soldr must build the UI dependency examples");
    assert!(
        output.status.success(),
        "UI dependency build failed: {}",
        String::from_utf8_lossy(&output.stderr[output.stderr.len().saturating_sub(4000)..])
    );
    let messages: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let core = messages
        .iter()
        .find_map(|message| {
            if message["reason"] != "compiler-artifact"
                || message["target"]["name"] != "zccache_core"
            {
                return None;
            }
            message["filenames"]
                .as_array()?
                .iter()
                .filter_map(|name| name.as_str())
                .find(|name| name.ends_with(".rlib"))
                .map(std::path::PathBuf::from)
        })
        .expect("Cargo JSON must identify the actual core dependency artifact");
    // Cargo's per-crate build-dir layout puts each dependency in its own
    // directory, so search every artifact's directory (proc macros included).
    let mut flags: Vec<String> = messages
        .iter()
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter_map(|message| message["filenames"].as_array())
        .flatten()
        .filter_map(|value| value.as_str())
        .filter(|path| {
            [".rlib", ".rmeta", ".so", ".dylib", ".dll"]
                .iter()
                .any(|extension| path.ends_with(extension))
        })
        .filter_map(|path| std::path::Path::new(path).parent())
        .map(|dir| format!("-Ldependency={}", dir.display()))
        .collect();
    flags.sort();
    flags.dedup();
    flags.insert(0, "--edition=2021".into());
    flags.push(format!("--extern=zccache_core={}", core.display()));
    flags
}
