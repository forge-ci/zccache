//! #1976: the decisions behind "live PID, endpoint gone", driven with real
//! processes, lock records and listeners. Everything here also compiles and runs
//! on Windows, where the endpoint is a named pipe.

use super::*;
use crate::cli::recovery::{breaker_reason_if_open, open_breaker};
use crate::ipc::{
    current_backend_identity, lock_file_path_for, read_lock_file_pid_for, unique_test_endpoint,
    write_backend_identity_for, write_lock_file_for,
};
use kernal_api::daemon_identity::DaemonIdentity;

const HOLDER_ENV: &str = "ZCCACHE_TEST_1976_HOLD_PID";
const HOLDER_LIFETIME: Duration = Duration::from_secs(60);

/// Not a test: the long-lived process the tests below point a lock file at. It
/// is this test binary, so its record verifies exactly as a real daemon's would.
#[test]
#[ignore = "helper process for the #1976 tests"]
fn pid_holder_process() {
    if std::env::var_os(HOLDER_ENV).is_some() {
        std::thread::sleep(HOLDER_LIFETIME);
    }
}

struct PidHolder(std::process::Child);

impl PidHolder {
    fn start() -> Self {
        let module = module_path!().split_once("::").expect("crate path").1;
        let child = std::process::Command::new(std::env::current_exe().expect("own exe"))
            .args([
                "--exact",
                &format!("{module}::pid_holder_process"),
                "--ignored",
                "--nocapture",
            ])
            .env(HOLDER_ENV, "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn pid holder");
        Self(child)
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn has_exited(&mut self) -> bool {
        self.0.try_wait().expect("poll pid holder").is_some()
    }

    /// Wait out the OS reaping a process that is expected to have been killed.
    fn exits_within(&mut self, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if self.has_exited() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        self.has_exited()
    }
}

impl Drop for PidHolder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A record saying `pid` is this test binary, with `digest` as its hash.
fn seed_record(endpoint: &str, pid: u32, digest: Option<[u8; 32]>) {
    let mut record = current_backend_identity(endpoint)
        .expect("capture own identity")
        .to_record();
    record.pid = pid;
    if let Some(digest) = digest {
        record.blake3_digest = digest;
    }
    write_lock_file_for(endpoint, pid).expect("seed lock");
    write_backend_identity_for(endpoint, &DaemonIdentity::from_record(record))
        .expect("seed identity");
}

fn slot_path(endpoint: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}.spawn", lock_file_path_for(endpoint).display()))
}

/// The hung daemon: its record proves it, the endpoint is gone. It must be
/// stopped, and its lock with it, so that a successor can take the cache root.
#[tokio::test]
async fn a_proven_instance_whose_endpoint_is_gone_is_stopped() {
    let endpoint = unique_test_endpoint();
    let mut holder = PidHolder::start();
    seed_record(&endpoint, holder.pid(), None);

    let retirement = retire_unreachable_instance(&endpoint, holder.pid()).await;

    assert_eq!(
        retirement,
        Retirement::Stopped { pid: holder.pid() },
        "the recorded instance is live and verified"
    );
    assert!(holder.exits_within(Duration::from_secs(5)), "must be dead");
    assert_eq!(read_lock_file_pid_for(&endpoint), None, "lock must go too");
}

/// The reused PID with no record to convict it: never kill what cannot be proven.
#[tokio::test]
async fn an_unrecorded_live_pid_is_never_killed() {
    let endpoint = unique_test_endpoint();
    let mut holder = PidHolder::start();
    write_lock_file_for(&endpoint, holder.pid()).expect("seed lock");

    let retirement = retire_unreachable_instance(&endpoint, holder.pid()).await;

    assert_eq!(retirement, Retirement::Unproven);
    assert!(!holder.has_exited(), "an unproven PID must be left running");
    assert_eq!(read_lock_file_pid_for(&endpoint), Some(holder.pid()));
    crate::ipc::remove_lock_file_for(&endpoint);
}

/// A daemon whose binary was replaced in place: the record names the PID but the
/// digest no longer proves it. Same answer as no record at all.
#[tokio::test]
async fn a_changed_binary_does_not_authorise_a_kill() {
    let endpoint = unique_test_endpoint();
    let mut holder = PidHolder::start();
    seed_record(&endpoint, holder.pid(), Some([7u8; 32]));

    let retirement = retire_unreachable_instance(&endpoint, holder.pid()).await;

    assert_eq!(retirement, Retirement::Unproven);
    assert!(!holder.has_exited());
    crate::ipc::remove_lock_file_for(&endpoint);
    crate::ipc::remove_backend_identity_for(&endpoint);
}

/// The old loop kept polling a PID that had already exited, then reported that
/// it "exists". The first probe after the exit must end the wait.
#[tokio::test]
async fn a_pid_that_exits_during_the_wait_ends_the_poll_early() {
    let endpoint = unique_test_endpoint();
    let mut holder = PidHolder::start();
    let pid = holder.pid();
    holder.0.kill().expect("kill holder");
    holder.0.wait().expect("reap holder");

    let started = Instant::now();
    let outcome = poll_unreachable_daemon(&endpoint, pid).await;

    assert!(matches!(outcome, UnreachablePoll::ProcessExited));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "took {:?}",
        started.elapsed()
    );
}

/// The race in the issue: one client gives up while the daemon is coming up. Its
/// failure is not an outage, and the marker it would have written fails the rest.
#[tokio::test]
async fn a_failure_after_the_daemon_came_up_is_a_success() {
    let endpoint = unique_test_endpoint();
    let _listener = crate::ipc::IpcListener::bind(&endpoint).expect("bind");
    let holder = PidHolder::start();
    seed_record(&endpoint, holder.pid(), None);

    let outcome = settle_failed_recovery(&endpoint, "gave up".to_string()).await;

    assert_eq!(outcome, Ok(()));
    assert_eq!(breaker_reason_if_open(&endpoint), None);
}

/// #1976 / #2084: a recorded daemon that accepts but stalls on `Status` is busy,
/// not gone. It must not count as a failure, and the check that guards the
/// retirement of the instance must see it as up.
#[tokio::test]
async fn a_recorded_daemon_stalling_on_status_is_preserved() {
    let endpoint = unique_test_endpoint();
    let mut listener = crate::ipc::IpcListener::bind(&endpoint).expect("bind");
    let stall = tokio::spawn(async move {
        let _held = listener.accept().await;
        std::future::pending::<()>().await;
    });
    let mut holder = PidHolder::start();
    seed_record(&endpoint, holder.pid(), None);

    assert!(recorded_daemon_accepts(&endpoint).await);
    let outcome = settle_failed_recovery(&endpoint, "gave up".to_string()).await;

    stall.abort();
    assert_eq!(outcome, Ok(()));
    assert_eq!(breaker_reason_if_open(&endpoint), None);
    assert!(!holder.has_exited(), "a busy daemon must not be stopped");
}

/// The ready-wait does accept a daemon that answers.
#[tokio::test]
async fn a_daemon_that_answers_is_ready() {
    let endpoint = unique_test_endpoint();
    let listener = crate::ipc::IpcListener::bind(&endpoint).expect("bind");
    let server = tokio::spawn(answer_one_status(listener, endpoint.clone()));

    let ready = super::super::wait_for_daemon_ready(&endpoint).await;

    assert_eq!(ready, Ok(()));
    server.await.expect("status server");
}

/// #2129: a loser of the cache-root race listens without ever serving. That is
/// the outage it looks like, not a daemon that came up.
#[tokio::test]
async fn a_listener_that_never_answers_is_not_a_daemon() {
    let endpoint = unique_test_endpoint();
    let _listener = crate::ipc::IpcListener::bind(&endpoint).expect("bind");

    let outcome = settle_failed_recovery(&endpoint, "gave up".to_string()).await;
    assert_eq!(outcome, Err("gave up".to_string()));
    crate::cli::recovery::clear_breaker(&endpoint);

    let wait = AdaptiveWaitConfig {
        poll_interval: Duration::from_millis(25),
        no_daemon_grace: Duration::from_millis(150),
        hard_ceiling: Duration::from_secs(5),
    };
    let error = super::super::wait_for_daemon_ready_with(&endpoint, |_| None, wait)
        .await
        .expect_err("a connect alone must not read as ready");
    assert!(error.contains("no daemon lockfile"), "{error}");
}

/// The one answer a real daemon gives first, so a test can tell it from a listener.
async fn answer_one_status(mut listener: crate::ipc::IpcListener, endpoint: String) {
    use crate::protocol::wire_prost::{self, zccache_v1};

    let mut conn = listener.accept().await.expect("accept");
    let request: Option<crate::protocol::DecodedWireMessage<zccache_v1::Request>> =
        conn.recv_wire().await.expect("recv");
    let Some(crate::protocol::DecodedWireMessage::ProstV16(request)) = request else {
        panic!("expected a prost status request");
    };
    let response = crate::protocol::Response::Status(crate::protocol::DaemonStatus {
        version: crate::core::VERSION.to_string(),
        daemon_namespace: "test".to_string(),
        endpoint,
        private_daemon: crate::protocol::PrivateDaemonStatus::shared(),
        artifact_count: 0,
        cache_size_bytes: 0,
        metadata_entries: 0,
        uptime_secs: 1,
        cache_hits: 0,
        cache_misses: 0,
        total_compilations: 0,
        non_cacheable: 0,
        compile_errors: 0,
        compile_errors_cached: 0,
        time_saved_ms: 0,
        total_links: 0,
        link_hits: 0,
        link_misses: 0,
        link_non_cacheable: 0,
        dep_graph_contexts: 0,
        dep_graph_files: 0,
        sessions_total: 0,
        sessions_active: 0,
        cache_dir: std::env::temp_dir().into(),
        dep_graph_version: 0,
        dep_graph_disk_size: 0,
        dep_graph_persisted: false,
        watcher_active: true,
        watcher_degradations: 0,
        index_writer_gone: false,
        materialization: crate::protocol::MaterializationStatus {
            mode: "COPY".to_string(),
            reflink: 0,
            hardlink: 0,
            copy: 0,
            reflink_fallbacks: 0,
        },
    });
    let response = wire_prost::supported_control_response_to_prost(&response, &request.request_id)
        .expect("encode status");
    conn.send_prost(&response).await.expect("send");
}

/// A parked client failing while the winner still holds the slot fails alone.
#[tokio::test]
async fn a_failure_behind_another_clients_spawn_does_not_open_the_breaker() {
    let endpoint = unique_test_endpoint();
    let _winner = acquire_spawn_slot(&endpoint).expect("win the slot");

    let outcome = settle_failed_recovery(&endpoint, "parked out".to_string()).await;

    assert_eq!(outcome, Err("parked out".to_string()), "still exit 125");
    assert_eq!(breaker_reason_if_open(&endpoint), None);
}

/// A real outage: no daemon, nobody spawning. #1170's breaker must still open.
#[tokio::test]
async fn a_real_outage_still_opens_the_breaker() {
    let endpoint = unique_test_endpoint();

    let outcome = settle_failed_recovery(&endpoint, "really down".to_string()).await;

    assert_eq!(outcome, Err("really down".to_string()));
    assert_eq!(
        breaker_reason_if_open(&endpoint).as_deref(),
        Some("really down")
    );
    crate::cli::recovery::clear_breaker(&endpoint);
}

/// A marker left by an attempt that failed must not outlive the outage. It used
/// to be read first, so a healthy daemon sat behind it for the whole cool-down.
#[tokio::test]
async fn an_open_breaker_does_not_refuse_a_reachable_daemon() {
    let endpoint = unique_test_endpoint();
    open_breaker(&endpoint, "one failed attempt");
    assert!(breaker_refusal(&endpoint).await.is_some(), "control: down");

    let _listener = crate::ipc::IpcListener::bind(&endpoint).expect("bind");

    assert_eq!(breaker_refusal(&endpoint).await, None);
    crate::cli::recovery::clear_breaker(&endpoint);
}

/// A slow spawn is someone else's to declare failed: the parked client keeps
/// waiting past its grace for as long as the winner holds the slot.
#[tokio::test]
async fn a_parked_client_outlasts_the_grace_while_the_slot_is_held() {
    let cfg = AdaptiveWaitConfig {
        poll_interval: Duration::from_millis(10),
        no_daemon_grace: Duration::from_millis(30),
        hard_ceiling: Duration::from_secs(5),
    };
    let endpoint = unique_test_endpoint();
    let started = Instant::now();
    let held_for = Duration::from_millis(300);

    let error = super::super::wait_for_daemon_ready_while(
        &endpoint,
        |_| None,
        || started.elapsed() < held_for,
        cfg,
    )
    .await
    .expect_err("nothing ever binds");

    assert!(error.contains("no daemon lockfile"), "wrong error: {error}");
    assert!(started.elapsed() >= held_for, "gave up behind the winner");
}

/// Stopping a daemon clears its slot file; the client holding the slot must get
/// it back, or a racer wins a second spawn between the stop and the spawn.
#[test]
fn the_slot_survives_the_stop_of_the_daemon_it_replaces() {
    let endpoint = unique_test_endpoint();
    let slot = acquire_spawn_slot(&endpoint).expect("win the slot");
    crate::cli::recovery::clear_stale_daemon_state(&endpoint);
    assert!(!slot_path(&endpoint).exists(), "control: cleared");

    slot.reassert();

    assert!(acquire_spawn_slot(&endpoint).is_none(), "racer must park");
    drop(slot);
    assert!(!slot_path(&endpoint).exists());
}
