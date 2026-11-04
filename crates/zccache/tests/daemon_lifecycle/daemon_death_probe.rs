//! Exercise the opt-in CI observer with real foreground process exits (#2064).
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

fn observe(command: &str, duration: &str, directory: &Path) -> (i32, Vec<serde_json::Value>) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ci/daemon_death_probe.py");
    let mut child = Command::new("uv")
        .args(["run", "--offline", "--no-project", "python"])
        .arg(script)
        .arg("--output")
        .arg(directory)
        .args(["--duration-secs", duration, "--interval-secs", "0.05"])
        .args(["--", "/bin/sh", "-c", command])
        .stdin(Stdio::null())
        .spawn()
        .expect("start CI observer");
    let status = zccache::core::wait::child_for(&mut child, Duration::from_secs(30))
        .expect("observer must finish within cleanup budget");
    let records = std::fs::read_to_string(directory.join("samples.jsonl"))
        .expect("durable observer records")
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid JSON record"))
        .collect();
    (status.code().expect("observer returns a status"), records)
}

#[test]
fn signal_death_retains_signal_without_claiming_oom() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, records) = observe("kill -KILL $$", "10", &tmp.path().join("signal"));
    assert_eq!(code, 137);
    let exit = records.last().unwrap();
    assert_eq!(exit["event"], "exited");
    assert_eq!(exit["returncode"], -9);
    assert_eq!(exit["termination_signal"], 9);
    assert!(exit["observer_stop_reason"].is_null());
    assert!(!records
        .iter()
        .any(|record| record["event"] == "observer-stop"));
}

#[test]
fn ordinary_failure_retains_status_and_daemon_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("ordinary");
    let (code, records) = observe("echo diagnostic >&2; exit 23", "10", &output);
    assert_eq!(code, 23);
    let exit = records.last().unwrap();
    assert_eq!(exit["returncode"], 23);
    assert!(exit["termination_signal"].is_null());
    assert!(exit["observer_stop_reason"].is_null());
    assert_eq!(
        std::fs::read_to_string(output.join("daemon.log")).unwrap(),
        "diagnostic\n"
    );
}

#[test]
fn observer_deadline_labels_its_own_termination() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, records) = observe("exec sleep 20", "0.2", &tmp.path().join("deadline"));
    assert_eq!(code, 124);
    assert!(records.iter().any(|record| record["event"] == "sample"));
    assert!(records
        .iter()
        .any(|record| record["event"] == "observer-stop"));
    let exit = records.last().unwrap();
    assert_eq!(exit["observer_stop_reason"], "observer-deadline");
    assert_eq!(exit["termination_signal"], 15);
}
