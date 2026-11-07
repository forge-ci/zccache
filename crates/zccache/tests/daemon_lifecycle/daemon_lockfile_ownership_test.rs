//! Regression test for zackees/zccache#1903 — a daemon that cannot record its
//! ownership must not report a successful start.
//!
//! The daemon binds its endpoint inside `DaemonServer::bind`, and the ownership
//! lock file is written *after* the bind wins. The bug was that a failure to
//! write that lock file was only logged: the daemon kept serving while being
//! undiscoverable and unstoppable, so `zccache stop` reported "stopped" while a
//! live process kept holding the cache-root writer lock.
//!
//! The failure is forced by making the lock-file path a **directory**. The
//! parent cache root stays a real writable directory, so the bind succeeds and
//! startup genuinely reaches the lock write; `write_lock_file`'s
//! `std::fs::write` then fails (`IsADirectory` on Unix, an access error on
//! Windows). Portable, no privileges required, and it does not disturb the bind
//! the way pointing `ZCCACHE_CACHE_DIR` at an unwritable path would.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unwrap_in_result
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use zccache_test_support::test_child::{Readiness, TestChild};

/// Per-spawn hang detector, deliberately far above any real latency. The
/// assertion is the exit status; this only stops the poll loop from spinning
/// forever when a daemon ignores the lock-write failure and serves forever —
/// which is exactly the `main` behavior this test exists to catch.
const HARD_CAP: Duration = Duration::from_secs(30);

/// Unique to this test so it cannot contend with the sibling
/// `daemon_spawn_lockfile_budget_test` namespaces (`"lockfile-budget"`) or any
/// developer's own daemon.
const TEST_DAEMON_NAMESPACE: &str = "lockfile-ownership";

#[test]
fn daemon_with_unwritable_lock_file_exits_instead_of_serving() {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let cache_dir = tmp.path().join("cache");
    std::fs::create_dir_all(&cache_dir).expect("create cache dir");

    // Chosen first because the ownership record it must block is scoped to
    // this endpoint (#1904).
    let endpoint = zccache::ipc::unique_test_endpoint();

    // The ownership record this daemon must never clobber or remove: a
    // directory where the endpoint-scoped lock file is expected.
    let lock_path = lock_file_path_for_cache_dir(&cache_dir, TEST_DAEMON_NAMESPACE, &endpoint);
    std::fs::create_dir_all(&lock_path).expect("occupy lock path with a directory");
    assert!(
        lock_path.is_dir(),
        "lock path must be a directory for this test to force a write failure"
    );

    let log_path = tmp.path().join("daemon.log");

    let mut command = Command::new(env!("CARGO_BIN_EXE_zccache-daemon"));
    command
        .args(["--foreground", "--endpoint", &endpoint])
        // The daemon detaches inherited stdio very early on the foreground
        // path unless it is handed a `--log-file`; without this the piped
        // stderr below is closed and the refusal message is lost.
        .args(["--log-file", log_path.to_str().expect("utf-8 log path")])
        .env("ZCCACHE_CACHE_DIR", &cache_dir)
        // Development binaries synthesize a hash namespace when this is
        // absent. Supply an explicit isolated identity so the parent and the
        // daemon agree on the ownership lockfile (#1404).
        .env("ZCCACHE_DAEMON_NAMESPACE", TEST_DAEMON_NAMESPACE)
        .env_remove("ZCCACHE_COLOCATE")
        .env("ZCCACHE_NO_UNLOCK", "1");
    let mut child = TestChild::spawn(command).expect("spawn daemon");

    let readiness = child.wait_until(HARD_CAP, || false).expect("poll child");
    // Always collect before any assertion: it guarantees a failing assertion
    // can never leak a live daemon still holding the cache-root writer lock.
    let collected = child.collect();
    let status = match readiness {
        Readiness::Exited(code) => Some(code),
        Readiness::Ready | Readiness::TimedOut => None,
    };
    let piped_stderr = collected.stderr_text();
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    let captured = format!("{piped_stderr}\n--- daemon log ---\n{log}");

    let Some(status) = status else {
        panic!(
            "daemon was still serving after {HARD_CAP:?} with an unwritable lock file.            On main it ignores the write failure and runs forever — see zackees/zccache#1903.            cache_dir={}, lock_path={}, endpoint={}            captured output:\n{}",
            cache_dir.display(),
            lock_path.display(),
            endpoint,
            captured.trim(),
        );
    };

    assert!(
        status != 0,
        "daemon reported a successful start (exit {status}) without an ownership record.            It is undiscoverable and unstoppable — `zccache stop` claims to have            stopped it while it keeps the cache-root writer lock. See            zackees/zccache#1903. Captured output:\n{}",
        captured.trim(),
    );

    assert!(
        lock_path.is_dir(),
        "the daemon must not remove or clobber an ownership record it never            wrote (removing a lock file this process does not own is zccache#1905).            lock_path={} is no longer a directory. Captured output:\n{}",
        lock_path.display(),
        captured.trim(),
    );

    // The exit status alone would also be satisfied by an unrelated early
    // failure, so pin the refusal itself: "refusing to serve" is the phrase
    // unique to the #1903 arm, and it only reaches a log the test asked for.
    assert!(
        captured.contains("refusing to serve"),
        "the daemon must exit through the lock-file refusal arm, not some other\n            failure. lock_path={}\n            captured output:\n{}",
        lock_path.display(),
        captured.trim(),
    );
}

/// Resolve the ownership lockfile path the daemon would write for
/// `cache_dir` / `namespace`.
///
/// `fn`-private to `daemon_spawn_lockfile_budget_test`, so this module keeps its
/// own copy: `zccache::ipc::lock_file_path_for()` derives the path from the
/// process-global environment rather than taking its cache root as an
/// argument. The env swap is held under the binary-wide
/// [`crate::LOCKFILE_ENV_LOCK`] for its duration.
///
/// The endpoint is part of the path (#1904): the daemon this test spawns on a
/// unique endpoint writes its ownership record under *that* endpoint's scope
/// tag, so blocking only the default endpoint's historical name would let the
/// daemon write a different file and serve — exactly what
/// `daemon_with_unwritable_lock_file_exits_instead_of_serving` caught.
fn lock_file_path_for_cache_dir(cache_dir: &Path, namespace: &str, endpoint: &str) -> PathBuf {
    let _lock = crate::LOCKFILE_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let prev_cache_dir = std::env::var_os("ZCCACHE_CACHE_DIR");
    let prev_namespace = std::env::var_os("ZCCACHE_DAEMON_NAMESPACE");
    let prev_colocate = std::env::var_os("ZCCACHE_COLOCATE");
    unsafe {
        std::env::set_var("ZCCACHE_CACHE_DIR", cache_dir);
        std::env::set_var("ZCCACHE_DAEMON_NAMESPACE", namespace);
        std::env::remove_var("ZCCACHE_COLOCATE");
    }
    let lockfile = zccache::ipc::lock_file_path_for(endpoint)
        .as_path()
        .to_path_buf();
    unsafe {
        restore_env("ZCCACHE_CACHE_DIR", prev_cache_dir);
        restore_env("ZCCACHE_DAEMON_NAMESPACE", prev_namespace);
        restore_env("ZCCACHE_COLOCATE", prev_colocate);
    }
    lockfile
}

unsafe fn restore_env(key: &str, value: Option<std::ffi::OsString>) {
    match value {
        Some(v) => unsafe { std::env::set_var(key, v) },
        None => unsafe { std::env::remove_var(key) },
    }
}
