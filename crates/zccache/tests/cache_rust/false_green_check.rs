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
    Command::new("soldr")
        .args(["cargo", verb])
        .env("SOLDR_ALLOW_UNPINNED", "0")
        .env("SOLDR_PORCELAIN", "0")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
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
    edit_after_test_build("check", "AUTO", "project", false);
}

#[test]
#[ignore = "integration: real Soldr/Cargo, watcher and materialization matrix"]
fn edit_after_test_build_matrix_without_watcher_delivery() {
    for verb in ["check", "build"] {
        for mode in ["LINK", "COPY"] {
            // The watcher ignores .cache components. No event can rescue the
            // freshness decision in those cells; ordinary paths race delivery.
            for project in ["project", ".cache/project"] {
                edit_after_test_build(verb, mode, project, true);
            }
        }
    }
}

fn edit_after_test_build(verb: &str, mode: &str, project_name: &str, prime: bool) {
    let fixture = tempfile::tempdir().expect("fixture");
    let project = fixture.path().join(project_name);
    let cache = fixture.path().join("cache");
    let target = project.join("target");
    std::fs::create_dir_all(project.join("src")).expect("create project");
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname='agent_mutation_repro'\nversion='0.1.0'\nedition='2021'\n",
    )
    .expect("manifest");
    std::fs::write(
        project.join("rust-toolchain.toml"),
        include_str!("../../../../rust-toolchain.toml"),
    )
    .expect("toolchain pin");
    let source = project.join("src/lib.rs");
    std::fs::write(&source, GOOD).expect("source");

    let zccache = env!("CARGO_BIN_EXE_zccache");
    let namespace = format!("i2002t-{}", std::process::id());
    let envs = [
        ("SOLDR_RUSTC_WRAPPER", PathBuf::from(zccache)),
        ("SOLDR_ZCCACHE_MODE", PathBuf::from(mode)),
        ("ZCCACHE_CACHE_DIR", cache.clone()),
        ("ZCCACHE_DAEMON_NAMESPACE", PathBuf::from(namespace)),
    ];
    let _daemon = Daemon { envs: &envs };

    let warm = cargo(&project, "test", &target, &envs);
    assert!(warm.status.success(), "warm test failed: {warm:?}");

    if prime {
        // Populate the selected path, then force Cargo to ask the cache again.
        // Require a hit before editing, including the ignored-watcher cells.
        let populate = cargo(&project, verb, &target, &envs);
        assert!(populate.status.success(), "populate failed: {populate:?}");
        let clean = cargo(&project, "clean", &target, &envs);
        assert!(clean.status.success(), "clean failed: {clean:?}");
        let replay = cargo(&project, verb, &target, &envs);
        assert!(replay.status.success(), "replay failed: {replay:?}");
        let mut outcomes = Vec::new();
        journal_outcomes(&cache, &mut outcomes);
        assert_eq!(
            outcomes.last().map(String::as_str),
            Some("hit"),
            "unchanged {verb}/{mode}/{project_name}: {outcomes:?}"
        );
    }

    // No sleep: ignored paths never deliver an event, ordinary paths race it.
    std::fs::write(&source, BROKEN).expect("edit");
    let checked = cargo(&project, verb, &target, &envs);
    let control = cargo(
        &project,
        verb,
        &fixture.path().join("control-target"),
        &[("SOLDR_RUSTC_WRAPPER", PathBuf::from("none"))],
    );
    let stderr = String::from_utf8_lossy(&checked.stderr);
    assert_eq!(checked.status.code(), control.status.code(), "{stderr}");
    assert_eq!(checked.status.code(), Some(101), "false green: {stderr}");
    for needle in ["E0425", "missing_agent_fixture_symbol", "src/lib.rs"] {
        assert!(stderr.contains(needle), "missing {needle}: {stderr}");
        assert!(String::from_utf8_lossy(&control.stderr).contains(needle));
    }

    let mut outcomes = Vec::new();
    journal_outcomes(&cache, &mut outcomes);
    assert_eq!(
        outcomes.last().map(String::as_str),
        Some("error"),
        "edited {verb}/{mode}/{project_name}: {outcomes:?}"
    );
}
