//! Get real-core UI link inputs without parsing Cargo's wrapped compiler argv.

pub(super) fn core_flags() -> Vec<String> {
    let toolchain = std::env::var("RUSTUP_TOOLCHAIN").expect("UI toolchain must be explicit");
    let output = std::process::Command::new("soldr")
        // Match ci.yml's DYLINT_CARGO_SHIM: this is a new managed toolchain
        // invocation, not a recursive front-door dispatch. Keep the compiler
        // wrapper and explicit nightly toolchain intact.
        .env_remove("IN_SOLDR_PID")
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
    let mut flags = vec!["--edition=2021".into()];
    flags.extend(dependency_dirs(&messages));
    flags.push(format!("--extern=zccache_core={}", core.display()));
    flags
}

/// One `-Ldependency` per artifact directory: Cargo's new build-dir layout
/// gives every unit its own output directory, so the core rlib's parent no
/// longer holds its transitive dependencies (rlibs and proc-macro dylibs).
fn dependency_dirs(messages: &[serde_json::Value]) -> Vec<String> {
    let mut dirs = std::collections::BTreeSet::new();
    for message in messages {
        if message["reason"] != "compiler-artifact" {
            continue;
        }
        for name in message["filenames"].as_array().into_iter().flatten() {
            if let Some(dir) = name
                .as_str()
                .and_then(|name| std::path::Path::new(name).parent())
            {
                dirs.insert(dir.to_path_buf());
            }
        }
    }
    dirs.into_iter()
        .map(|dir| format!("-Ldependency={}", dir.display()))
        .collect()
}
