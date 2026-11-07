//! Regression coverage for #1904: the daemon lock file and the lifecycle
//! helpers that consult it must be scoped to the endpoint they describe.
//!
//! Before the fix, `lock_file_path()` / `backend_identity_path()` /
//! `check_running_daemon()` all derived from `default_endpoint()` and never
//! consulted `ZCCACHE_ENDPOINT`, so a daemon started on a custom endpoint
//! overwrote the default daemon's PID in one shared ownership record. That
//! produced two user-visible failures:
//!
//! - **Failure A** — `zccache stop` (default endpoint) force-killed the
//!   *custom* daemon, because the lock it read named the wrong PID.
//! - **Failure B** — `check_running_daemon()` retired the *default*
//!   endpoint's socket while managing a custom one, wedging a live daemon
//!   out of reach of every client.
//!
//! A cache root admits one writer (`CacheRootWriterLock`, #1162), so two live
//! daemons can never share the root these records live in. These tests run one
//! real daemon at a time, and a planted record stands in for the other
//! endpoint's owner where one is needed. #2129 is the case where the second
//! daemon is refused: `start` must report that rather than exit 0.
//!
//! These tests spawn the real CLI binary and real daemons, so they are
//! `#[ignore]`-gated like their neighbours in this directory.
//!
//! Run with:
//!
//! ```text
//! cargo nextest run --test daemon_lifecycle -E 'test(/^daemon_endpoint_scoped_lock_test::/)'
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unwrap_in_result
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Daemon namespace pinned by this module. A development build otherwise
/// derives a `<version>-<exe hash>` namespace (#1362 / #1394) that this test
/// process does not share, so a lock path computed here would not be the one
/// the CLI reads — and, worse, the test could collide with the developer's own
/// daemon. Mirrors `FORCED_STOP_NAMESPACE` in `tests/cli/cli_daemon_start.rs`.
const ENDPOINT_SCOPE_NAMESPACE: &str = "endpoint-scope";

/// Locate the `zccache` binary next to this test executable.
///
/// Returns `None` when it is absent so a caller can skip instead of failing:
/// this module is `#[ignore]`-gated and not every environment builds the
/// multi-call binary first.
fn zccache_bin() -> Option<PathBuf> {
    let mut path = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("parent of test binary")
        .parent()
        .expect("target dir")
        .to_path_buf();

    path.push(if cfg!(windows) {
        "zccache.exe"
    } else {
        "zccache"
    });

    path.exists().then_some(path)
}

/// One isolated cache root, two endpoints.
///
/// `ZCCACHE_CACHE_DIR` and `ZCCACHE_DAEMON_NAMESPACE` are pinned on every
/// child command so nothing here can reach the developer's real daemon or
/// their real cache root. `Drop` stops both endpoints: a leaked daemon holds
/// `CacheRootWriterLock` for its cache root.
struct TwoEndpoints {
    bin: PathBuf,
    /// `ZCCACHE_CACHE_DIR`, shared by both endpoints.
    cache_dir: tempfile::TempDir,
    /// Holds the custom endpoint's socket so its path stays inside a directory
    /// this test owns and removes on drop.
    _endpoint_dir: tempfile::TempDir,
    /// Fixed for the scenario's lifetime: `start`, `stop` and the lock name must
    /// all see one value. A named pipe on Windows, a socket path elsewhere.
    custom_endpoint: String,
}

impl TwoEndpoints {
    fn new(bin: PathBuf) -> Self {
        let endpoint_dir = tempfile::Builder::new()
            .prefix("zccache-endpoint-scope-")
            .tempdir()
            .expect("endpoint dir tempdir");
        let custom_endpoint = if cfg!(windows) {
            let unique = zccache::core::stable_path_id(endpoint_dir.path());
            format!(r"\\.\pipe\zccache-{ENDPOINT_SCOPE_NAMESPACE}-{unique}")
        } else {
            endpoint_dir
                .path()
                .join("custom.sock")
                .to_string_lossy()
                .into_owned()
        };
        Self {
            bin,
            cache_dir: tempfile::Builder::new()
                .prefix("zccache-endpoint-scope-")
                .tempdir()
                .expect("cache dir tempdir"),
            _endpoint_dir: endpoint_dir,
            custom_endpoint,
        }
    }

    /// `<cache root>/v<VERSION>` — the directory both lock files live in once
    /// `ZCCACHE_CACHE_DIR` is set (`normalized_override_root` folds the
    /// version tag into the effective root, #1003).
    fn version_dir(&self) -> PathBuf {
        self.cache_dir
            .path()
            .join(zccache::core::config::versioned_subdir())
    }

    /// The default endpoint's ownership record. The default endpoint carries an
    /// empty scope tag, so its historical filename is byte-for-byte unchanged.
    fn default_lock_path(&self) -> PathBuf {
        let version = zccache::core::config::versioned_subdir();
        self.version_dir()
            .join(format!("daemon-{ENDPOINT_SCOPE_NAMESPACE}-{version}.lock"))
    }

    /// The custom endpoint's ownership record — same directory as
    /// [`Self::default_lock_path`], distinguished only by the endpoint scope
    /// tag `stable_path_id` folds into the name. Deriving the name here rather
    /// than calling `zccache::ipc::lock_file_path_for` is deliberate: that
    /// function reads `ZCCACHE_CACHE_DIR` / `ZCCACHE_DAEMON_NAMESPACE` from
    /// *this* process, which the test never sets (it pins them per child
    /// command), so it would resolve against the developer's real cache root.
    fn custom_lock_path(&self) -> PathBuf {
        let version = zccache::core::config::versioned_subdir();
        let scope = zccache::core::stable_path_id(Path::new(&self.custom_endpoint));
        self.version_dir().join(format!(
            "daemon-{ENDPOINT_SCOPE_NAMESPACE}-{version}-{scope}.lock"
        ))
    }

    /// A `zccache` invocation pinned to the isolated cache root + namespace and
    /// to `endpoint`, or to the default endpoint when `None`.
    fn run(&self, subcommand: &str, endpoint: Option<&str>) -> Output {
        let mut cmd = Command::new(&self.bin);
        cmd.arg(subcommand)
            .env("ZCCACHE_CACHE_DIR", self.cache_dir.path())
            .env("ZCCACHE_DAEMON_NAMESPACE", ENDPOINT_SCOPE_NAMESPACE)
            .env_remove("ZCCACHE_ENDPOINT");
        if let Some(endpoint) = endpoint {
            cmd.env("ZCCACHE_ENDPOINT", endpoint);
        }
        cmd.stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .unwrap_or_else(|error| panic!("run zccache {subcommand}: {error}"))
    }

    /// `start`, which must succeed, leaving a live daemon named by `lock`.
    fn start_expecting_daemon(&self, endpoint: Option<&str>, lock: &Path) -> u32 {
        let output = self.run("start", endpoint);
        assert!(
            output.status.success(),
            "zccache start failed at {endpoint:?}: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            self.diagnostics()
        );
        self.wait_for_lock(lock, &output)
    }

    fn stop(&self, endpoint: Option<&str>) {
        let output = self.run("stop", endpoint);
        assert!(
            output.status.success(),
            "zccache stop failed at {endpoint:?}: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            self.diagnostics()
        );
    }

    /// Stand in for the other endpoint's owner: a record naming a live process
    /// that is not a daemon. Nothing managing this test's endpoint may touch it.
    fn plant_default_record(&self) -> u32 {
        let pid = std::process::id();
        std::fs::create_dir_all(self.version_dir()).expect("create version dir");
        std::fs::write(self.default_lock_path(), pid.to_string()).expect("plant default record");
        pid
    }

    /// Poll until `path` names a PID. `start` has already had a daemon answer,
    /// and the daemon records itself before it serves, so a timeout is a defect.
    fn wait_for_lock(&self, path: &Path, start: &Output) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(pid) = read_lock_pid(path) {
                return pid;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the daemon lock at {}\nzccache start stderr: {}\n{}",
                path.display(),
                String::from_utf8_lossy(&start.stderr),
                self.diagnostics()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Directory listing plus the tail of every daemon log, for a panic message.
    fn diagnostics(&self) -> String {
        let mut report = format!("version dir {}:", self.version_dir().display());
        for entry in walk_files(&self.version_dir()) {
            report.push_str(&format!("\n  {}", entry.display()));
            if entry.extension().is_some_and(|ext| ext == "log") {
                let text = std::fs::read_to_string(&entry).unwrap_or_default();
                let lines: Vec<&str> = text.lines().collect();
                for line in &lines[lines.len().saturating_sub(5)..] {
                    report.push_str(&format!("\n    {line}"));
                }
            }
        }
        report
    }

    /// Every daemon lock currently in the versioned cache directory.
    ///
    /// Dotfiles are skipped: the cache root's single-writer claim is
    /// `.writer.lock` (`CacheRootWriterLock`), whose `Path::extension` is also
    /// `lock` even though it is not a daemon ownership record.
    fn lock_files(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.version_dir()) else {
            return Vec::new();
        };
        let mut locks: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "lock")
                    && !path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            })
            .collect();
        locks.sort();
        locks
    }

    /// The default endpoint's socket, discovered rather than predicted. Any
    /// `*.sock` in the versioned cache directory is the default daemon's; the
    /// custom endpoint's is in this test's own endpoint tempdir. Empty on
    /// Windows, where the endpoint is a named pipe, and for a long cache root,
    /// whose endpoint falls back to a compact `/tmp` path.
    fn default_socket_paths(&self) -> Vec<PathBuf> {
        let mut sockets: Vec<PathBuf> = walk_files(&self.version_dir())
            .into_iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "sock"))
            .collect();
        sockets.sort();
        sockets
    }
}

impl Drop for TwoEndpoints {
    fn drop(&mut self) {
        // A planted record names this test process: never offer it to `stop`.
        if read_lock_pid(&self.default_lock_path()) == Some(std::process::id()) {
            let _ = std::fs::remove_file(self.default_lock_path());
        }
        let _ = self.run("stop", Some(&self.custom_endpoint));
        let _ = self.run("stop", None);
    }
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

fn read_lock_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()
}

fn wait_until_dead(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while zccache::ipc::is_process_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Each endpoint keeps its own ownership record.
///
/// Before the fix there was exactly one lock file in the cache root and a
/// `start` overwrote whichever PID it held, so the two records could never name
/// two different live processes.
#[test]
#[ignore] // integration-level: starts a real daemon + spawns the CLI binary
fn two_endpoints_keep_independent_lock_files() {
    let Some(bin) = zccache_bin() else {
        eprintln!("skipping: zccache binary not built (run `cargo build` first)");
        return;
    };
    let scenario = TwoEndpoints::new(bin);

    let default_pid = scenario.plant_default_record();
    let custom_pid = scenario.start_expecting_daemon(
        Some(&scenario.custom_endpoint),
        &scenario.custom_lock_path(),
    );

    let locks = scenario.lock_files();
    assert_eq!(
        locks.len(),
        2,
        "two endpoints must own two distinct lock records, found {locks:?} in {}",
        scenario.version_dir().display()
    );
    assert!(
        locks.contains(&scenario.default_lock_path()),
        "the default endpoint's lock {} is missing from {locks:?}",
        scenario.default_lock_path().display()
    );
    assert!(
        locks.contains(&scenario.custom_lock_path()),
        "the custom endpoint's lock {} is missing from {locks:?}",
        scenario.custom_lock_path().display()
    );

    assert_ne!(
        default_pid, custom_pid,
        "both lock records name the same PID {default_pid} — the custom daemon \
         overwrote the default endpoint's ownership record"
    );
    assert_eq!(
        read_lock_pid(&scenario.default_lock_path()),
        Some(default_pid),
        "starting the custom endpoint rewrote the default endpoint's record"
    );
    assert!(
        zccache::ipc::is_process_alive(custom_pid),
        "the custom endpoint's daemon {custom_pid} must still be running"
    );
}

/// `zccache stop` only ever terminates the daemon whose endpoint was named.
///
/// Failure A: with one shared lock record, `check_running_daemon()` on the
/// custom endpoint returned the *default* daemon's PID, and the forced-kill
/// path terminated it.
#[test]
#[ignore] // integration-level: starts real daemons + spawns the CLI binary
fn stop_only_terminates_the_daemon_at_its_own_endpoint() {
    let Some(bin) = zccache_bin() else {
        eprintln!("skipping: zccache binary not built (run `cargo build` first)");
        return;
    };
    let scenario = TwoEndpoints::new(bin);
    let custom = Some(scenario.custom_endpoint.as_str());

    // A live default daemon, and a stop naming an endpoint with no daemon.
    let default_pid = scenario.start_expecting_daemon(None, &scenario.default_lock_path());
    let sockets_before = scenario.default_socket_paths();

    scenario.stop(custom);

    assert!(
        zccache::ipc::is_process_alive(default_pid),
        "stop at the custom endpoint killed the DEFAULT endpoint's daemon \
         ({default_pid}) — the ownership record is not endpoint-scoped"
    );
    assert_eq!(
        read_lock_pid(&scenario.default_lock_path()),
        Some(default_pid),
        "stop at the custom endpoint disturbed the default endpoint's record"
    );
    // Failure B: retiring the default endpoint's socket out from under a live
    // daemon wedges it permanently (it keeps holding CacheRootWriterLock but
    // no client can ever connect again).
    assert_eq!(
        scenario.default_socket_paths(),
        sockets_before,
        "stopping the custom endpoint retired the default endpoint's socket \
         — a live daemon is now unreachable"
    );

    scenario.stop(None);
    wait_until_dead(default_pid);

    // A live custom daemon: the stop that names it must kill it and leave the
    // other endpoint's record alone.
    let planted = scenario.plant_default_record();
    let custom_pid = scenario.start_expecting_daemon(custom, &scenario.custom_lock_path());

    scenario.stop(custom);
    wait_until_dead(custom_pid);

    assert!(
        !zccache::ipc::is_process_alive(custom_pid),
        "stop at the custom endpoint must terminate that daemon ({custom_pid})"
    );
    assert_eq!(
        read_lock_pid(&scenario.default_lock_path()),
        Some(planted),
        "stop at the custom endpoint disturbed the default endpoint's record"
    );
}

/// #2129: a daemon refused by a live cache-root writer is not a started daemon.
///
/// It binds its endpoint, loses the root and retries, so its listener comes and
/// goes for several seconds. `start` used to read one connect to that listener as
/// success. The daemon then won the root as soon as the holder stopped, which
/// left it running behind a `stop` that had reported nothing there.
#[test]
#[ignore] // integration-level: starts real daemons + spawns the CLI binary
fn start_reports_failure_when_another_daemon_holds_the_cache_root() {
    let Some(bin) = zccache_bin() else {
        eprintln!("skipping: zccache binary not built (run `cargo build` first)");
        return;
    };
    let scenario = TwoEndpoints::new(bin);
    let custom = Some(scenario.custom_endpoint.as_str());

    let default_pid = scenario.start_expecting_daemon(None, &scenario.default_lock_path());

    let refused = scenario.run("start", custom);
    assert!(
        !refused.status.success(),
        "start reported success, but the cache root is held by {default_pid}\n{}",
        scenario.diagnostics()
    );
    assert_eq!(
        scenario.lock_files(),
        vec![scenario.default_lock_path()],
        "only the writer may hold a lock record\n{}",
        scenario.diagnostics()
    );
    assert!(
        zccache::ipc::is_process_alive(default_pid),
        "the refused start must leave the writer {default_pid} running"
    );

    scenario.stop(custom);
    scenario.stop(None);
    wait_until_dead(default_pid);

    // Nothing may be left waiting to take the root over.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        assert_eq!(
            read_lock_pid(&scenario.custom_lock_path()),
            None,
            "the refused daemon outlived its failed start and took the root\n{}",
            scenario.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
