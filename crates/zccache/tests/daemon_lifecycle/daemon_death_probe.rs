//! Exercise the opt-in CI observer with real foreground process exits (#2064).
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn observe(
    command: &str,
    duration: &str,
    directory: &Path,
    interrupt: bool,
) -> (i32, Vec<serde_json::Value>) {
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
    if interrupt {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !std::fs::read_to_string(directory.join("samples.jsonl"))
            .unwrap_or_default()
            .contains("\"event\": \"sample\"")
        {
            assert!(Instant::now() < deadline, "observer readiness deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut sender = Command::new("/bin/kill")
            .args(["-TERM", &child.id().to_string()])
            .spawn()
            .expect("signal observer through uv");
        assert!(
            zccache::core::wait::child_for(&mut sender, Duration::from_secs(5))
                .expect("signal delivery deadline")
                .success()
        );
    }
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
    let (code, records) = observe("kill -KILL $$", "10", &tmp.path().join("signal"), false);
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
    let (code, records) = observe("echo diagnostic >&2; exit 23", "10", &output, false);
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
    let (code, records) = observe("exec sleep 20", "0.2", &tmp.path().join("deadline"), false);
    assert_eq!(code, 124);
    assert!(records.iter().any(|record| record["event"] == "sample"));
    assert!(records
        .iter()
        .any(|record| record["event"] == "observer-stop"));
    let exit = records.last().unwrap();
    assert_eq!(exit["observer_stop_reason"], "observer-deadline");
    assert_eq!(exit["termination_signal"], 15);
}

#[test]
fn supervisor_term_is_recorded_as_observer_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    let (code, records) = observe("exec sleep 20", "15", &tmp.path().join("interrupt"), true);
    // uv may expose its own forwarded signal status instead of Python's 130.
    assert!(matches!(code, 130 | 143));
    assert!(records
        .iter()
        .any(|record| record["event"] == "observer-stop"));
    let exit = records.last().unwrap();
    assert_eq!(exit["event"], "exited");
    assert_eq!(exit["observer_stop_reason"], "observer-interrupt");
    assert_eq!(exit["termination_signal"], 15);
}
