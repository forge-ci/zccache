//! Get real-core UI link inputs without parsing Cargo's wrapped compiler argv.

pub(super) fn dependency_flags() -> Vec<String> {
    let toolchain = std::env::var("RUSTUP_TOOLCHAIN").expect("UI toolchain must be explicit");
    let output = std::process::Command::new("soldr")
        // Match ci.yml's DYLINT_CARGO_SHIM: this is a new managed toolchain
        // invocation, not a recursive front-door dispatch. Keep the compiler
        // wrapper and explicit nightly toolchain intact.
        .env_remove("IN_SOLDR_PID")
        // A private target dir keeps `-Ldependency` to one copy of each
        // crate. Shared with the Dylint library build, it also held a second
        // `thiserror` rlib, and rustc refused the ambiguous `kernal_api` (E0460).
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
    let mut flags = vec!["--edition=2021".into()];
    // Every transitive artifact's directory, not only the three named crates':
    // a rustc-dev sysroot ships its own `thiserror` and friends, and rustc
    // must see the copies these crates were built against (E0460).
    let mut search: Vec<String> = messages
        .iter()
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter_map(|message| message["filenames"].as_array())
        .flatten()
        .filter_map(|value| value.as_str())
        // Proc macros (`thiserror_impl`) are shared objects, not rlibs.
        .filter(|path| {
            [".rlib", ".rmeta", ".so", ".dylib", ".dll"]
                .iter()
                .any(|extension| path.ends_with(extension))
        })
        .filter_map(|path| std::path::Path::new(path).parent())
        .map(|dir| format!("-Ldependency={}", dir.display()))
        .collect();
    search.sort();
    search.dedup();
    flags.extend(search);
    for name in ["zccache_core", "kernal_api", "tokio"] {
        let artifact = messages
            .iter()
            .find_map(|message| {
                if message["reason"] != "compiler-artifact" || message["target"]["name"] != name {
                    return None;
                }
                message["filenames"]
                    .as_array()?
                    .iter()
                    .filter_map(|value| value.as_str())
                    .find(|path| path.ends_with(".rlib"))
            })
            .unwrap_or_else(|| panic!("Cargo JSON missing {name} artifact"));
        let artifact = std::path::Path::new(artifact);
        flags.push(format!(
            "-Ldependency={}",
            artifact.parent().unwrap().display()
        ));
        flags.push(format!("--extern={name}={}", artifact.display()));
    }
    flags
}
