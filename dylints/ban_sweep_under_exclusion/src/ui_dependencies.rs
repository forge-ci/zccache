//! Build real facade inputs through soldr with an explicit finite budget.
use std::time::Duration;
pub(super) fn dependency_flags(bound: Duration) -> Vec<String> {
    let toolchain = std::env::var("RUSTUP_TOOLCHAIN").expect("explicit UI toolchain");
    let mut spec = zccache_core::wait::CommandSpec::new("soldr")
        .clear_env(true)
        .args([
            "rustup",
            "run",
            &toolchain,
            "cargo",
            "build",
            "--examples",
            "--message-format=json",
        ]);
    // SpawnSpec 0.1.26 has clear-and-override, rather than env_remove.
    // Preserve the toolchain/wrapper environment and omit only the recursion marker.
    for (key, value) in std::env::vars_os() {
        if !key.to_string_lossy().eq_ignore_ascii_case("IN_SOLDR_PID") {
            spec = spec.env(key, value);
        }
    }
    let output = zccache_core::wait::output_for(spec, bound).expect("bounded UI dependency build");
    assert_eq!(
        output.exit.raw_code(),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let mut flags = vec!["--edition=2021".into()];
    // Newer Cargo keeps intermediate outputs in per-package directories.
    // Supplying only each facade's directory hides its transitive crates from
    // rustc. Discover every search directory from the same Cargo JSON build.
    let directories: std::collections::BTreeSet<_> = messages
        .iter()
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter_map(|message| message["filenames"].as_array())
        .flatten()
        .filter_map(|filename| filename.as_str())
        .filter_map(|filename| std::path::Path::new(filename).parent())
        .collect();
    for directory in directories {
        flags.push(format!("-Ldependency={}", directory.display()));
    }
    for name in [
        "zccache_core",
        "zccache_artifact",
        "zccache_daemon_core",
        "kernal_api",
    ] {
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
                    .find(|name| name.ends_with(".rlib"))
            })
            .expect("Cargo JSON must identify the real facade artifact");
        let artifact = std::path::Path::new(artifact);
        flags.push(format!("--extern={name}={}", artifact.display()));
    }
    flags
}
