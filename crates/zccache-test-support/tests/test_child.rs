//! Contract tests for the owned test child and the nextest termination proof
//! (#1971). Children are this test binary re-executed with a role, so the
//! fixtures need no platform-specific shell tools.
//!
//! The `planted_*` tests do nothing unless `ci/check_nextest_termination.py`
//! plants them: they stay `#[ignore]`d and also inert under `--include-ignored`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use zccache_core::wait::child_for;
use zccache_test_support::test_child::{Readiness, TestChild};

const ROLE_ENV: &str = "ZCCACHE_TEST_CHILD_ROLE";
const PID_FILE_ENV: &str = "ZCCACHE_TEST_CHILD_PID_FILE";
const PLANT_DIR_ENV: &str = "ZCCACHE_TEST_PLANT_DIR";
/// Well beyond the Linux 64 KiB, macOS and Windows default pipe buffers.
const FLOOD_BYTES: usize = 2 * 1024 * 1024;
const BOUND: Duration = Duration::from_secs(60);
const HANG: Duration = Duration::from_secs(300);

fn role_command(role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "child_role_entry",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE_ENV, role);
    command
}

fn flood(mut sink: impl Write, byte: u8) {
    let chunk = vec![byte; 64 * 1024];
    for _ in 0..FLOOD_BYTES / chunk.len() {
        sink.write_all(&chunk).unwrap();
    }
    sink.flush().unwrap();
}

/// Not a test of its own: the entry point of every re-executed child.
#[test]
fn child_role_entry() {
    let Ok(role) = std::env::var(ROLE_ENV) else {
        return;
    };
    match role.as_str() {
        "flood" => {
            flood(std::io::stdout().lock(), b'o');
            flood(std::io::stderr().lock(), b'e');
        }
        "flood-then-hang" => {
            flood(std::io::stdout().lock(), b'o');
            std::thread::sleep(HANG);
        }
        "hang" => std::thread::sleep(HANG),
        "spawn-descendant-then-exit" | "spawn-descendant-then-hang" => {
            // The descendant stays in this group and inherits both pipes.
            let descendant = role_command("hang").spawn().unwrap();
            let pid_file = std::env::var_os(PID_FILE_ENV).unwrap();
            std::fs::write(pid_file, descendant.id().to_string()).unwrap();
            if role == "spawn-descendant-then-hang" {
                std::thread::sleep(HANG);
            }
            std::mem::forget(descendant);
        }
        other => panic!("unknown child role {other}"),
    }
}

fn read_pid(path: &Path) -> u32 {
    let deadline = Instant::now() + BOUND;
    loop {
        if let Some(pid) = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| text.parse().ok())
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "no pid in {}", path.display());
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether `pid` still runs; a zombie awaiting its reaper does not count.
fn alive(pid: u32) -> bool {
    kernal_api::platform::host_processes::snapshot()
        .iter()
        .any(|process| process.pid() == pid && process.running())
}

fn assert_exits(pid: u32) {
    let deadline = Instant::now() + BOUND;
    while alive(pid) {
        assert!(
            Instant::now() < deadline,
            "descendant {pid} outlived its owner"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn pid_file_command(role: &str) -> (Command, tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("descendant.pid");
    let mut command = role_command(role);
    command.env(PID_FILE_ENV, &pid_file);
    (command, dir, pid_file)
}

#[test]
fn old_wait_before_drain_shape_wedges_as_a_controlled_negative() {
    let mut command = role_command("flood");
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    // Nothing reads the pipes, so the child fills one and blocks until killed.
    let error = child_for(&mut child, Duration::from_secs(3)).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
}

#[test]
fn output_far_beyond_both_pipe_buffers_completes_and_is_retained() {
    let output = TestChild::spawn(role_command("flood"))
        .unwrap()
        .finish(BOUND)
        .unwrap();
    assert_eq!(output.code, Some(0), "stderr: {}", output.stderr.len());
    // libtest's banner also contains the letter, hence the lower bound. The
    // flood is under the default retention, so every byte must survive.
    let count = |bytes: &[u8], byte| bytes.iter().filter(|seen| **seen == byte).count();
    assert!(count(&output.stdout, b'o') >= FLOOD_BYTES);
    assert_eq!(count(&output.stderr, b'e'), FLOOD_BYTES);
    assert!(
        output.reader_errors.is_empty(),
        "{:?}",
        output.reader_errors
    );
    assert!(!output.pipes_retained);
}

#[test]
fn retention_is_bounded_while_the_child_keeps_writing() {
    let output = TestChild::spawn_with_retention(role_command("flood"), 1024)
        .unwrap()
        .finish(BOUND)
        .unwrap();
    assert_eq!(output.code, Some(0));
    assert_eq!(output.stderr.len(), 1024);
    assert_eq!(output.stderr_total, FLOOD_BYTES as u64);
}

#[test]
fn a_missed_deadline_terminates_and_retains_the_captured_tail() {
    let started = Instant::now();
    let error = TestChild::spawn(role_command("flood-then-hang"))
        .unwrap()
        .finish(Duration::from_secs(5))
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("ooo"), "tail was lost");
    assert!(started.elapsed() < HANG / 2);
}

#[test]
fn a_spawn_failure_is_an_error_not_a_child() {
    let missing = tempfile::tempdir().unwrap().path().join("no-such-program");
    assert!(TestChild::spawn(Command::new(missing)).is_err());
}

#[test]
fn readiness_reports_ready_timeout_and_early_exit() {
    let mut hanging = TestChild::spawn(role_command("hang")).unwrap();
    assert_eq!(
        hanging.wait_until(BOUND, || true).unwrap(),
        Readiness::Ready
    );
    let expired = hanging.wait_until(Duration::from_millis(50), || false);
    assert_eq!(expired.unwrap(), Readiness::TimedOut);
    assert!(
        hanging.try_wait().unwrap().is_none(),
        "expiry must not kill"
    );
    drop(hanging);

    let mut exiting = TestChild::spawn(role_command("flood")).unwrap();
    let readiness = exiting.wait_until(BOUND, || false).unwrap();
    assert_eq!(readiness, Readiness::Exited(0));
}

#[test]
fn dropping_the_owner_cleans_up_the_process_tree() {
    let (command, _dir, pid_file) = pid_file_command("spawn-descendant-then-hang");
    let child = TestChild::spawn(command).unwrap();
    let leader = child.id();
    let descendant = read_pid(&pid_file);
    assert!(alive(descendant));
    drop(child);
    assert_exits(descendant);
    assert_exits(leader);
}

#[test]
fn descendant_retained_pipes_do_not_outlive_collect() {
    let (command, _dir, pid_file) = pid_file_command("spawn-descendant-then-exit");
    let mut child = TestChild::spawn(command).unwrap();
    assert_eq!(child.wait_for(BOUND).unwrap(), 0);
    let descendant = read_pid(&pid_file);
    let output = child.collect();
    assert!(!output.pipes_retained, "group kill must release the pipes");
    assert_eq!(output.code, Some(0));
    assert_exits(descendant);
}

fn plant_dir() -> Option<PathBuf> {
    std::env::var_os(PLANT_DIR_ENV).map(PathBuf::from)
}

/// Hangs on purpose: nextest must terminate it by name, keep this diagnostic,
/// and take the descendants down. Both pids go to the plant dir for the check.
#[test]
#[ignore = "planted by ci/check_nextest_termination.py"]
fn planted_hang_is_terminated_by_name() {
    let Some(dir) = plant_dir() else {
        return;
    };
    println!("PLANTED-DIAGNOSTIC: retained by the harness");
    let raw_pid_file = dir.join("raw.pid");
    let mut raw = role_command("hang");
    raw.stdout(Stdio::null()).stderr(Stdio::null());
    let raw = raw.spawn().unwrap();
    std::fs::write(&raw_pid_file, raw.id().to_string()).unwrap();
    let owned = TestChild::spawn(role_command("hang")).unwrap();
    std::fs::write(dir.join("owned.pid"), owned.id().to_string()).unwrap();
    std::thread::sleep(HANG);
    std::mem::forget((raw, owned));
}

/// Passes beside the hang: a timeout must not abort unrelated tests.
#[test]
#[ignore = "planted by ci/check_nextest_termination.py"]
fn planted_unrelated_test_still_passes() {}
