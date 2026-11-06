//! A respawn racing the old daemon's exit (#2106).
//!
//! The bind lock lasts as long as the old process, so a replacement started in
//! that window loses the bind. On a clean exit the daemon removes its lock file
//! first, so nothing tells the client an old instance is still leaving. The
//! window is normally brief; these tests stretch it with SIGSTOP, using the
//! state a client sees in it: no lock record, nothing connectable, bind lock held.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unwrap_in_result
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const NAMESPACE: &str = "exit-window";
const HARD_CAP: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(25);

struct Fixture {
    _tmp: tempfile::TempDir,
    cache: PathBuf,
    endpoint: String,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        Self {
            cache,
            endpoint: zccache::ipc::unique_test_endpoint(),
            _tmp: tmp,
        }
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env("ZCCACHE_CACHE_DIR", &self.cache)
            .env("ZCCACHE_DAEMON_NAMESPACE", NAMESPACE)
            .env("ZCCACHE_ENDPOINT", &self.endpoint)
            .env_remove("ZCCACHE_SESSION_ID")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn spawn_daemon(&self) -> KillOnDrop {
        KillOnDrop(
            self.command(env!("CARGO_BIN_EXE_zccache-daemon"))
                .args(["--foreground", "--endpoint", &self.endpoint])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn daemon"),
        )
    }

    fn lock_records(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        collect_lock_records(&self.cache, &mut found);
        found
    }

    fn wait_until_recorded(&self) {
        let deadline = Instant::now() + HARD_CAP;
        while self.lock_records().is_empty() || !Path::new(&self.endpoint).exists() {
            assert!(Instant::now() < deadline, "daemon never recorded itself");
            std::thread::sleep(POLL);
        }
    }

    /// Freeze a running daemon in the client-visible state of its exit window.
    fn freeze_in_exit_window(&self, daemon: &KillOnDrop) {
        signal(daemon.0.id(), "-STOP");
        for record in self.lock_records() {
            std::fs::remove_file(record).unwrap();
        }
        std::fs::remove_file(&self.endpoint).unwrap();
    }

    fn wrapper_status(&self) -> (Option<i32>, String) {
        let mut wrapper = self
            .command(env!("CARGO_BIN_EXE_zccache"))
            .args([shim().to_str().unwrap(), "0"])
            .spawn()
            .expect("spawn wrapper");
        let status = wait_bounded(&mut wrapper);
        let output = wrapper.wait_with_output().unwrap();
        (
            status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self
            .command(env!("CARGO_BIN_EXE_zccache"))
            .arg("stop")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn collect_lock_records(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_lock_records(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "lock")
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("daemon"))
        {
            found.push(path);
        }
    }
}

fn signal(pid: u32, signal: &str) {
    let status = Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .status()
        .expect("run kill");
    assert!(status.success());
}

fn wait_bounded(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + HARD_CAP;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("process outlived {HARD_CAP:?}");
        }
        std::thread::sleep(POLL);
    }
}

fn shim() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.push("echo_shim");
    path
}

/// The contract: once the old daemon is gone, the client has a working daemon.
#[test]
fn client_gets_a_daemon_when_its_respawn_races_the_old_daemons_exit() {
    let fixture = Fixture::new();
    let mut old = fixture.spawn_daemon();
    fixture.wait_until_recorded();
    fixture.freeze_in_exit_window(&old);

    let old_pid = old.0.id();
    let leaves = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        signal(old_pid, "-KILL");
    });
    let started = Instant::now();
    let (code, stderr) = fixture.wrapper_status();
    let elapsed = started.elapsed();
    leaves.join().unwrap();
    old.0.wait().unwrap();

    assert_eq!(
        code,
        Some(0),
        "wrapper failed after {elapsed:?} once the old daemon was gone: {stderr}"
    );
}

/// The retry must not disturb the old rule: a healthy winner means defer now.
#[test]
fn loser_defers_at_once_to_a_healthy_daemon() {
    let fixture = Fixture::new();
    let _winner = fixture.spawn_daemon();
    fixture.wait_until_recorded();

    let started = Instant::now();
    let mut loser = fixture.spawn_daemon();
    let status = wait_bounded(&mut loser.0);

    assert_eq!(status.code(), Some(0));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!fixture.lock_records().is_empty(), "winner's record lost");
}

/// A holder that never leaves must not keep the loser running forever.
#[test]
fn loser_gives_up_on_a_holder_that_never_leaves() {
    let fixture = Fixture::new();
    let old = fixture.spawn_daemon();
    fixture.wait_until_recorded();
    fixture.freeze_in_exit_window(&old);

    let mut loser = fixture.spawn_daemon();
    let status = wait_bounded(&mut loser.0);

    assert_eq!(status.code(), Some(0), "the loser still defers with exit 0");
    assert!(Path::new(&fixture.endpoint).symlink_metadata().is_err());
}
