//! #1970: identity probes retain the helper's stderr as a bounded, redacted
//! diagnostic in the lifecycle log, without touching the stdout identity.
//!
//! Fake compilers are real child processes, so the capture path under test is
//! the one the daemon runs.

use super::super::compiler_hash::{
    hash_cc_identity, hash_cc_identity_async, hash_rustc_identity, hash_rustc_identity_async,
};
use super::CacheDirEnvGuard;
use std::path::{Path, PathBuf};

fn fake_tool(dir: &Path, stdout: &str, stderr: &str, code: u8) -> PathBuf {
    let echo = |text: &str, redirect: &str| {
        if text.is_empty() {
            String::new()
        } else {
            format!("echo {text}{redirect}\n")
        }
    };
    let (name, header, stderr_redirect, exit) = if crate::platform::host::is_windows() {
        ("fake-tool.cmd", "@echo off\n", " 1>&2", "exit /b")
    } else {
        ("fake-tool", "#!/bin/sh\n", " >&2", "exit")
    };
    let path = dir.join(name);
    let body = format!(
        "{header}{}{}{exit} {code}\n",
        echo(stdout, ""),
        echo(stderr, stderr_redirect)
    );
    std::fs::write(&path, body).unwrap();
    crate::platform::fs::permissions::make_executable(&path).unwrap();
    path
}

fn lifecycle_log() -> String {
    std::fs::read_to_string(crate::core::lifecycle::log_file_path().as_path()).unwrap_or_default()
}

/// Events for tools under `dir`. Unguarded probe tests running on other threads
/// can write to the same process-global log.
fn diagnostics(dir: &Path) -> Vec<serde_json::Value> {
    let dir = dir.display().to_string();
    lifecycle_log()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| {
            event["event"] == "small_tool_diagnostic"
                && event["tool"]
                    .as_str()
                    .is_some_and(|tool| tool.contains(&dir))
        })
        .collect()
}

#[test]
fn failing_tool_stderr_is_retained_with_tool_identity_and_status() {
    let dir = tempfile::tempdir().unwrap();
    let _env = CacheDirEnvGuard::set(dir.path());
    let tool = fake_tool(dir.path(), "", "fatal: license-1970 expired", 7);

    assert!(
        hash_cc_identity(&tool).is_some(),
        "file-hash fallback holds"
    );

    let events = diagnostics(dir.path());
    assert_eq!(events.len(), 1, "{events:?}");
    let event = &events[0];
    assert!(event["tool"].as_str().unwrap().contains("fake-tool"));
    assert_eq!(event["exit_code"], 7);
    assert!(event["duration_ns"].as_u64().unwrap() > 0);
    assert!(event["stderr_excerpt"]
        .as_str()
        .unwrap()
        .contains("fatal: license-1970 expired"));
}

#[test]
fn async_probe_retains_failing_tool_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let _env = CacheDirEnvGuard::set(dir.path());
    let tool = fake_tool(dir.path(), "", "async-1970 failure", 9);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert!(runtime.block_on(hash_cc_identity_async(tool)).is_some());

    let events = diagnostics(dir.path());
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["exit_code"], 9);
    assert!(events[0]["stderr_excerpt"]
        .as_str()
        .unwrap()
        .contains("async-1970 failure"));
}

#[test]
fn successful_probe_keeps_stdout_identity_and_retains_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let _env = CacheDirEnvGuard::set(dir.path());
    let quiet = fake_tool(dir.path(), "rustc 1.95.0", "", 0);
    let quiet_hash = hash_rustc_identity(&quiet).unwrap();
    assert!(
        diagnostics(dir.path()).is_empty(),
        "silent success logs nothing"
    );

    let noisy = fake_tool(dir.path(), "rustc 1.95.0", "note: shim-1970 warming", 0);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let noisy_hash = runtime.block_on(hash_rustc_identity_async(noisy)).unwrap();

    assert_eq!(quiet_hash, noisy_hash, "stderr must not enter the identity");
    let events = diagnostics(dir.path());
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["exit_code"], 0);
    assert!(events[0]["stderr_excerpt"]
        .as_str()
        .unwrap()
        .contains("shim-1970"));
}

#[test]
fn secrets_in_tool_stderr_are_not_logged() {
    let dir = tempfile::tempdir().unwrap();
    let _env = CacheDirEnvGuard::set(dir.path());
    let tool = fake_tool(dir.path(), "", "denied API_TOKEN=hunter2-1970 refused", 1);

    hash_cc_identity(&tool);

    let log = lifecycle_log();
    assert!(log.contains("refused"), "{log}");
    assert!(!log.contains("hunter2-1970"), "{log}");
}

#[test]
fn spawn_failure_records_tool_and_error() {
    let dir = tempfile::tempdir().unwrap();
    let _env = CacheDirEnvGuard::set(dir.path());
    let missing = dir.path().join("zzz-missing-tool-1970");

    assert!(hash_cc_identity(&missing).is_none());

    let events = diagnostics(dir.path());
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(events[0]["tool"]
        .as_str()
        .unwrap()
        .contains("zzz-missing-tool-1970"));
    assert!(!events[0]["error"].as_str().unwrap().is_empty());
}
