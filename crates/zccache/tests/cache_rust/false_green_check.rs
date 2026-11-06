//! A size-changing source edit after a passing `cargo test` must not let
//! `cargo check` replay the old success (#2002).

#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const GOOD: &str = "#[test] fn ok() {}\n";
const BROKEN: &str = "pub fn broken() { missing_agent_fixture_symbol(); }\n";

/// Stops the fixture's daemon even when an assertion fails.
struct Daemon<'a> {
    envs: &'a [(&'static str, PathBuf)],
}

impl Drop for Daemon<'_> {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_zccache"))
            .arg("stop")
            .envs(self.envs.iter().map(|(name, value)| (*name, value)))
            .output();
    }
}

fn cargo(project: &Path, verb: &str, target: &Path, envs: &[(&str, PathBuf)]) -> Output {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    Command::new(cargo)
        .arg(verb)
        .current_dir(project)
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_TERM_COLOR", "never")
        .envs(envs.iter().map(|(name, value)| (*name, value)))
        .output()
        .unwrap_or_else(|error| panic!("cargo {verb} failed to start: {error}"))
}

fn journal_outcomes(dir: &Path, found: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read cache dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            journal_outcomes(&path, found);
        } else if path
            .file_name()
            .is_some_and(|n| n == "compile_journal.jsonl")
        {
            let text = std::fs::read_to_string(&path).expect("read journal");
            for line in text.lines().filter(|l| l.contains("agent_mutation_repro")) {
                let record: serde_json::Value = serde_json::from_str(line).expect("journal json");
                found.push(record["outcome"].as_str().expect("outcome").to_string());
            }
        }
    }
}

#[test]
#[ignore = "integration: builds a real crate through RUSTC_WRAPPER"]
fn edit_after_test_build_fails_check_like_uncached_control() {
    let fixture = tempfile::tempdir().expect("fixture");
    let project = fixture.path().join("project");
    let cache = fixture.path().join("cache");
    let target = project.join("target");
    std::fs::create_dir_all(project.join("src")).expect("create project");
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='agent_mutation_repro'\nversion='0.1.0'\nedition='2021'\n",
    )
    .expect("manifest");
    let source = project.join("src/lib.rs");
    std::fs::write(&source, GOOD).expect("source");

    let zccache = env!("CARGO_BIN_EXE_zccache");
    let namespace = format!("i2002t-{}", std::process::id());
    let envs = [
        ("RUSTC_WRAPPER", PathBuf::from(zccache)),
        ("ZCCACHE_CACHE_DIR", cache.clone()),
        ("ZCCACHE_DAEMON_NAMESPACE", PathBuf::from(namespace)),
    ];
    let _daemon = Daemon { envs: &envs };

    let warm = cargo(&project, "test", &target, &envs);
    assert!(warm.status.success(), "warm test failed: {warm:?}");

    // No sleep: the check races watcher delivery, as a real edit does.
    std::fs::write(&source, BROKEN).expect("edit");
    let checked = cargo(&project, "check", &target, &envs);
    let control = cargo(
        &project,
        "check",
        &fixture.path().join("control-target"),
        &[("RUSTC_WRAPPER", PathBuf::new())],
    );
    let stderr = String::from_utf8_lossy(&checked.stderr);
    assert_eq!(checked.status.code(), control.status.code(), "{stderr}");
    assert_eq!(checked.status.code(), Some(101), "false green: {stderr}");
    for needle in ["E0425", "missing_agent_fixture_symbol", "src/lib.rs"] {
        assert!(stderr.contains(needle), "missing {needle}: {stderr}");
        assert!(String::from_utf8_lossy(&control.stderr).contains(needle));
    }

    // Unchanged source must keep its valid hit, or "no cache" would pass above.
    std::fs::write(&source, GOOD).expect("restore");
    let clean = cargo(&project, "clean", &target, &envs);
    assert!(clean.status.success(), "clean failed: {clean:?}");
    let replay = cargo(&project, "check", &target, &envs);
    assert!(replay.status.success(), "replay failed: {replay:?}");

    let mut outcomes = Vec::new();
    journal_outcomes(&cache, &mut outcomes);
    assert!(outcomes.iter().any(|o| o == "error"), "{outcomes:?}");
    assert_eq!(
        outcomes.last().map(String::as_str),
        Some("hit"),
        "{outcomes:?}"
    );
}
