//! #2090: download-daemon liveness and recovery, driven with real processes,
//! records and listeners. Every test pins its own cache root under the env guard.

use super::*;
use crate::cli::commands::tests::daemon::IsolatedCacheRoot;
use crate::download_protocol::daemon_mgmt;
use crate::ipc::{
    current_backend_identity, read_backend_identity_for, unique_test_endpoint,
    write_backend_identity_for,
};
use kernal_api::daemon_identity::DaemonIdentity;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const HOLDER_ENV: &str = "ZCCACHE_TEST_2090_HOLD_PID";
const HOLDER_LIFETIME: Duration = Duration::from_secs(60);

/// Not a test: the live process the tests below point a lock file at.
#[test]
#[ignore = "helper process for the #2090 tests"]
fn download_pid_holder_process() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        std::thread::sleep(HOLDER_LIFETIME);
    }
}

/// Test-helper unwrap: this module denies `clippy::{unwrap,expect}_used`
/// outside `#[test]` bodies, and a helper that panics with context keeps the
/// failure message without a lint allow.
fn ok<T, E: std::fmt::Debug>(result: Result<T, E>, what: &str) -> T {
    match result {
        Ok(value) => value,
        Err(err) => panic!("{what}: {err:?}"),
    }
}

fn some<T>(value: Option<T>, what: &str) -> T {
    value.unwrap_or_else(|| panic!("{what}: was None"))
}

struct Holder {
    child: std::process::Child,
    exe: PathBuf,
}

impl Holder {
    /// A live process running this test binary as `exe`.
    fn start_as(exe: PathBuf) -> Self {
        let module = some(module_path!().split_once("::"), "crate path").1;
        let child = ok(
            std::process::Command::new(&exe)
                .args([
                    "--exact",
                    &format!("{module}::download_pid_holder_process"),
                    "--ignored",
                    "--nocapture",
                ])
                .env(HOLDER_ENV, "1")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn(),
            "spawn pid holder",
        );
        Self { child, exe }
    }

    /// A live process whose executable has the download daemon's exact name,
    /// which is all the old stem check could see.
    fn start_named_like_the_daemon(dir: &std::path::Path) -> Self {
        let exe = dir.join(crate::platform::executable::native_name(
            std::ffi::OsStr::new("zccache-download-daemon"),
        ));
        let own = ok(std::env::current_exe(), "own exe");
        if std::fs::hard_link(&own, &exe).is_err() {
            ok(
                std::fs::copy(&own, &exe),
                "copy test binary under the daemon name",
            );
        }
        Self::start_as(exe)
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn is_running(&mut self) -> bool {
        ok(self.child.try_wait(), "poll holder").is_none()
    }

    fn exits_within(&mut self, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if !self.is_running() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        !self.is_running()
    }

    /// Record this holder as `endpoint`'s daemon, as the daemon entry does.
    fn seed_record(&self, endpoint: &str) {
        let mut record = ok(current_backend_identity(endpoint), "capture own identity").to_record();
        record.pid = self.pid();
        record.executable_path.clone_from(&self.exe);
        ok(
            write_backend_identity_for(endpoint, &DaemonIdentity::from_record(record)),
            "seed identity",
        );
        ok(
            daemon_mgmt::write_lock_file_for(endpoint, self.pid()),
            "seed lock",
        );
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn own_exe_holder() -> Holder {
    Holder::start_as(ok(std::env::current_exe(), "own exe"))
}

fn dead_pid() -> u32 {
    let mut child = ok(
        std::process::Command::new(ok(std::env::current_exe(), "own exe"))
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .spawn(),
        "spawn short-lived process",
    );
    let pid = child.id();
    ok(child.wait(), "reap short-lived process");
    pid
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    ok(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build(),
        "runtime",
    )
    .block_on(future)
}

/// A real download daemon on `endpoint`, in this process.
fn start_real_daemon(endpoint: &str) -> std::thread::JoinHandle<()> {
    let endpoint = endpoint.to_string();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || {
        block_on(async move {
            let mut daemon = ok(
                crate::download_daemon::DownloadDaemon::bind(&endpoint),
                "bind replacement daemon",
            );
            ok(ready_tx.send(()), "signal daemon ready");
            ok(daemon.run().await, "run replacement daemon");
        });
    });
    ok(
        ready_rx.recv_timeout(Duration::from_secs(10)),
        "replacement daemon failed to bind",
    );
    thread
}

/// The custom client must find nothing to do with the default daemon's records.
#[test]
fn a_custom_endpoint_never_touches_the_default_lock_or_endpoint() {
    let _root = IsolatedCacheRoot::set();
    #[cfg(unix)]
    let default_endpoint = daemon_mgmt::default_endpoint();
    daemon_mgmt::write_lock_file(dead_pid()).unwrap();
    #[cfg(unix)]
    let _default_listener = {
        std::fs::create_dir_all(std::path::Path::new(&default_endpoint).parent().unwrap()).unwrap();
        std::os::unix::net::UnixListener::bind(&default_endpoint).unwrap()
    };

    let custom = unique_test_endpoint();
    assert_eq!(check_running_daemon_for(&custom), None);

    assert!(
        daemon_mgmt::read_lock_file_pid().is_some(),
        "a custom-endpoint check removed the default endpoint's lock"
    );
    #[cfg(unix)]
    assert!(
        std::path::Path::new(&default_endpoint).exists(),
        "a custom-endpoint check unlinked the default endpoint's socket"
    );
}

/// The live default daemon says nothing about a custom endpoint: reporting it
/// held every first custom-endpoint call in a 2 s wait and then failed it.
#[test]
fn a_live_default_daemon_does_not_answer_for_a_custom_endpoint() {
    let root = IsolatedCacheRoot::set();
    let holder = Holder::start_named_like_the_daemon(root.path());
    daemon_mgmt::write_lock_file(holder.pid()).unwrap();

    assert_eq!(check_running_daemon_for(&unique_test_endpoint()), None);
    assert_eq!(daemon_mgmt::read_lock_file_pid(), Some(holder.pid()));
}

#[test]
fn locks_are_scoped_to_their_endpoint() {
    let _root = IsolatedCacheRoot::set();
    let custom = unique_test_endpoint();
    assert_eq!(
        daemon_mgmt::lock_file_path_for(&daemon_mgmt::default_endpoint()),
        daemon_mgmt::lock_file_path(),
        "the default endpoint keeps the historical file name"
    );
    assert_ne!(
        daemon_mgmt::lock_file_path_for(&custom),
        daemon_mgmt::lock_file_path()
    );
}

/// #2035 for this daemon: a PID from another container's namespace names nothing
/// here, even when the process now holding it is another download daemon.
#[test]
fn a_record_from_another_pid_namespace_is_stale_despite_a_matching_name() {
    let root = IsolatedCacheRoot::set();
    let endpoint = unique_test_endpoint();
    let holder = Holder::start_named_like_the_daemon(root.path());
    holder.seed_record(&endpoint);
    let sibling = format!(
        "{}.pid-namespace",
        crate::ipc::backend_identity_path_for(&endpoint).display()
    );
    if !std::path::Path::new(&sibling).exists() {
        return; // No PID namespaces on this host.
    }
    std::fs::write(&sibling, "pid:[1]").unwrap();

    assert_eq!(check_running_daemon_for(&endpoint), None);
    assert_eq!(
        daemon_mgmt::read_lock_file_pid_for(&endpoint),
        None,
        "the stale record must retire itself"
    );
    assert!(read_backend_identity_for(&endpoint).is_none());
}

/// A daemon whose binary was replaced in place still serves its clients. The old
/// stem check read `name (deleted)` as a foreign process and retired its endpoint.
#[cfg(target_os = "linux")]
#[test]
fn a_live_daemon_whose_binary_was_deleted_is_not_retired() {
    let root = IsolatedCacheRoot::set();
    let endpoint = unique_test_endpoint();
    let holder = Holder::start_named_like_the_daemon(root.path());
    holder.seed_record(&endpoint);
    std::fs::remove_file(&holder.exe).unwrap();

    assert_eq!(check_running_daemon_for(&endpoint), Some(holder.pid()));
    assert_eq!(
        daemon_mgmt::read_lock_file_pid_for(&endpoint),
        Some(holder.pid())
    );
}

#[test]
fn a_dead_pid_on_a_custom_endpoint_cleans_only_that_endpoint() {
    let _root = IsolatedCacheRoot::set();
    let custom = unique_test_endpoint();
    daemon_mgmt::write_lock_file_for(&custom, dead_pid()).unwrap();
    assert_eq!(check_running_daemon_for(&custom), None);
    assert_eq!(daemon_mgmt::read_lock_file_pid_for(&custom), None);
}

/// The reported failure: every call failed after about 2 s, with no way out.
#[test]
fn a_proven_instance_whose_endpoint_is_gone_is_replaced() {
    let _root = IsolatedCacheRoot::set();
    let endpoint = unique_test_endpoint();
    let mut holder = own_exe_holder();
    holder.seed_record(&endpoint);

    let mut replacement = None;
    block_on(ensure_daemon_with(&endpoint, || {
        replacement = Some(start_real_daemon(&endpoint));
        Ok(())
    }))
    .expect("the replacement must serve this call");

    assert!(holder.exits_within(Duration::from_secs(5)), "must be dead");
    let client = DownloadClient::new(Some(endpoint));
    client.daemon_status().expect("replacement answers");
    client.stop_daemon().expect("stop replacement");
    replacement
        .expect("spawned")
        .join()
        .expect("replacement exits");
}

/// No record proves this PID is ours, so it must not be killed, and a successor
/// beside it would share its destinations. The caller gets a typed reason.
#[test]
fn an_unproven_live_pid_fails_fast_and_is_left_running() {
    let root = IsolatedCacheRoot::set();
    let endpoint = unique_test_endpoint();
    let mut holder = Holder::start_named_like_the_daemon(root.path());
    daemon_mgmt::write_lock_file_for(&endpoint, holder.pid()).unwrap();

    let spawned = AtomicBool::new(false);
    let error = block_on(ensure_daemon_with(&endpoint, || {
        spawned.store(true, Ordering::SeqCst);
        Ok(())
    }))
    .expect_err("an unproven PID has no safe recovery");

    assert!(matches!(
        error,
        EnsureDaemonError::UnprovenInstance { pid, .. } if pid == holder.pid()
    ));
    assert!(holder.is_running(), "an unproven PID must never be killed");
    assert!(!spawned.load(Ordering::SeqCst));
    assert_eq!(
        daemon_mgmt::read_lock_file_pid_for(&endpoint),
        Some(holder.pid())
    );
}

/// A record whose binary changed is not proof either.
#[test]
fn a_changed_binary_does_not_authorise_a_kill() {
    let _root = IsolatedCacheRoot::set();
    let endpoint = unique_test_endpoint();
    let mut holder = own_exe_holder();
    holder.seed_record(&endpoint);
    let mut record = read_backend_identity_for(&endpoint).unwrap().to_record();
    record.blake3_digest = [0xAB; 32];
    write_backend_identity_for(&endpoint, &DaemonIdentity::from_record(record)).unwrap();

    let error = block_on(ensure_daemon_with(&endpoint, || Ok(()))).expect_err("must refuse");

    assert!(matches!(error, EnsureDaemonError::UnprovenInstance { .. }));
    assert!(holder.is_running());
}

/// The no-spawn guard forbids a replacement, so nothing may be stopped: killing
/// the instance would leave a host with neither daemon.
#[test]
fn the_no_spawn_guard_refuses_before_any_kill() {
    let _root = IsolatedCacheRoot::set();
    let previous = std::env::var_os(crate::core::config::NO_SPAWN_ENV);
    std::env::set_var(crate::core::config::NO_SPAWN_ENV, "1");
    let endpoint = unique_test_endpoint();
    let mut holder = own_exe_holder();
    holder.seed_record(&endpoint);

    let error = block_on(ensure_daemon_with(&endpoint, || Ok(())));

    match previous {
        Some(value) => std::env::set_var(crate::core::config::NO_SPAWN_ENV, value),
        None => std::env::remove_var(crate::core::config::NO_SPAWN_ENV),
    }
    assert!(matches!(error, Err(EnsureDaemonError::Other(_))));
    assert!(holder.is_running());
}
