//! Recovery when the lock file names a live PID but the endpoint is gone
//! (#1976).
//!
//! The daemon binds its endpoint before it writes the lock file, and holds the
//! bound pipe instances until after it removes the lock. So "lock names a live
//! PID, endpoint not found" is never a slow start of that PID, and never its
//! orderly drain. A slow start has no lock yet and waits in the spawn path. What
//! is left is a transient gap, the daemon having exited during the wait, a daemon
//! whose listener is gone, or a PID nobody can prove is ours.
//!
//! Polling alone cannot resolve the last two, and it used to end by opening the
//! breaker over every other wrapper. After the poll one client takes the spawn
//! slot and replaces the instance. It kills only an instance the record proves;
//! an unproven PID is left running, and a successor is tried beside it, because
//! the first-instance pipe flag and the cache-root writer lock already refuse an
//! overlapping daemon.

use super::{
    acquire_spawn_slot, check_daemon_version, connect_client, daemon_answers_status,
    probe_says_daemon_is_merely_busy, spawn_and_wait, spawn_slot_in_flight, spawn_with_slot,
    stop_stale_daemon, stop_wedged_daemon, AdaptiveWaitConfig, VersionCheck,
};
use std::time::{Duration, Instant};

/// Number of probes of the endpoint before the client concludes it will not
/// come back, about 9 s with the backoff below.
const UNREACHABLE_PROBES: usize = 20;

/// The longest wait for a replacement. The poll above and this must fit inside
/// the 30 s recovery budget, so that an unresolvable PID reports its own reason
/// rather than "exceeded its budget".
const REPLACEMENT_CEILING: Duration = Duration::from_secs(15);

/// Bound on the connect that decides whether a breaker still applies. A Windows
/// client retries `ERROR_PIPE_BUSY` without limit, and the outage path must stay
/// cheap for every translation unit.
const BREAKER_CONNECT_PROBE: Duration = Duration::from_millis(250);

async fn endpoint_accepts_connections(endpoint: &str) -> bool {
    tokio::time::timeout(BREAKER_CONNECT_PROBE, connect_client(endpoint))
        .await
        .is_ok_and(|connected| connected.is_ok())
}

/// The reason to refuse this invocation, if the breaker is open and the endpoint
/// is still not accepting. A connect that succeeds means the outage is over, so
/// a marker from an earlier failed attempt must not fail the build; the ladder
/// that follows clears it. No request is sent, so this adds no roundtrip.
pub(super) async fn breaker_refusal(endpoint: &str) -> Option<String> {
    let reason = crate::cli::recovery::breaker_reason_if_open(endpoint)?;
    (!endpoint_accepts_connections(endpoint).await).then_some(reason)
}

/// Decide what one client's failed recovery means for the other clients.
///
/// Only a failure nobody can still repair opens the breaker. A daemon that came
/// up in the meantime and answers is a success; a listener alone is not (#2129). While another client holds the spawn slot,
/// this client's failure says nothing about that attempt: it fails alone, with
/// exit 125, and leaves the rest of the build free to use the result.
pub(super) async fn settle_failed_recovery(endpoint: &str, reason: String) -> Result<(), String> {
    if daemon_answers_status(endpoint).await {
        crate::cli::recovery::clear_breaker(endpoint);
        return Ok(());
    }
    if !spawn_slot_in_flight(endpoint) {
        crate::cli::recovery::open_breaker(endpoint, &reason);
    }
    Err(reason)
}

enum UnreachablePoll {
    /// A probe reached a conclusion, including a replacement it had to make.
    Settled(Result<(), String>),
    /// The recorded PID exited during the wait; nothing is left to replace.
    ProcessExited,
    /// The endpoint never came back and the PID is still alive.
    Exhausted,
}

/// Probe the endpoint with backoff, watching the recorded PID between probes.
async fn poll_unreachable_daemon(endpoint: &str, pid: u32) -> UnreachablePoll {
    let mut backoff = Duration::from_millis(100);
    for _ in 0..UNREACHABLE_PROBES {
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_millis(500));
        // Re-read every iteration: a daemon replaced legitimately between
        // attempts is a different instance, and the kill must follow.
        let attempt_instance = crate::ipc::read_backend_identity_for(endpoint);
        match check_daemon_version(endpoint).await {
            VersionCheck::Ok | VersionCheck::DaemonNewer => {
                return UnreachablePoll::Settled(Ok(()));
            }
            VersionCheck::DaemonOlder { daemon_ver } => {
                tracing::info!(
                    daemon_ver,
                    client_ver = crate::core::VERSION,
                    "daemon is older than client during startup, auto-recovering"
                );
                let killed_pid = stop_stale_daemon(endpoint, attempt_instance.as_ref()).await;
                return UnreachablePoll::Settled(
                    spawn_and_wait(
                        endpoint,
                        crate::core::lifecycle::REASON_REPLACED_STALE_VERSION,
                        killed_pid,
                    )
                    .await,
                );
            }
            VersionCheck::CommError => {
                // A daemon can be too busy to answer a 2 s Status ping; ask a
                // second, cheaper question before concluding it is dead (#1161).
                if probe_says_daemon_is_merely_busy(endpoint).await {
                    continue;
                }
                let killed_pid = stop_stale_daemon(endpoint, attempt_instance.as_ref()).await;
                return UnreachablePoll::Settled(
                    spawn_and_wait(
                        endpoint,
                        crate::core::lifecycle::REASON_REPLACED_COMM_ERROR,
                        killed_pid,
                    )
                    .await,
                );
            }
            VersionCheck::ClientConfigError(message) => {
                return UnreachablePoll::Settled(Err(message));
            }
            VersionCheck::Unreachable => {
                if !crate::ipc::is_process_alive(pid) {
                    return UnreachablePoll::ProcessExited;
                }
            }
        }
    }
    UnreachablePoll::Exhausted
}

/// What the client holding the spawn slot did about the recorded instance.
#[derive(Debug, PartialEq, Eq)]
enum Retirement {
    /// The record proved the instance and it is gone.
    Stopped { pid: u32 },
    /// Nothing proves this PID is our daemon, so it was not touched.
    Unproven,
    /// The record proved the instance, but it could not be stopped.
    StopFailed,
}

impl Retirement {
    fn label(&self) -> &'static str {
        match self {
            Self::Stopped { .. } => "stopped",
            Self::Unproven => "unproven-left-running",
            Self::StopFailed => "stop-failed",
        }
    }
}

/// Stop the instance only if its own record proves it is the live process.
async fn retire_unreachable_instance(endpoint: &str, pid: u32) -> Retirement {
    let Some(instance) = crate::ipc::verified_daemon_instance_for(endpoint, pid) else {
        return Retirement::Unproven;
    };
    match stop_wedged_daemon(endpoint, Some(&instance)).await {
        Some(pid) => Retirement::Stopped { pid },
        None => Retirement::StopFailed,
    }
}

/// Everything after [`check_running_daemon_for`](crate::ipc::check_running_daemon_for)
/// reported a live PID behind an unreachable endpoint.
pub(super) async fn recover_unreachable_daemon(endpoint: &str, pid: u32) -> Result<(), String> {
    let started = Instant::now();
    match poll_unreachable_daemon(endpoint, pid).await {
        UnreachablePoll::Settled(outcome) => return outcome,
        UnreachablePoll::ProcessExited => {
            // Clean up the dead record, then take the ordinary spawn path.
            let _ = crate::ipc::check_running_daemon_for(endpoint);
            return spawn_and_wait(
                endpoint,
                crate::core::lifecycle::REASON_REPLACED_UNREACHABLE,
                None,
            )
            .await;
        }
        UnreachablePoll::Exhausted => {}
    }

    let replacement = AdaptiveWaitConfig {
        hard_ceiling: REPLACEMENT_CEILING,
        ..AdaptiveWaitConfig::default()
    };
    let slot = acquire_spawn_slot(endpoint);
    let retirement = if slot.is_some() {
        // An earlier slot holder may have just finished; never stop its work.
        if daemon_answers_status(endpoint).await {
            return Ok(());
        }
        Some(retire_unreachable_instance(endpoint, pid).await)
    } else {
        None
    };
    if let Some(retirement) = &retirement {
        crate::core::lifecycle::write_event(
            crate::core::lifecycle::EVENT_DAEMON_UNREACHABLE_RECOVERY,
            serde_json::json!({
                "endpoint": endpoint,
                "pid": pid,
                "process_alive": crate::ipc::is_process_alive(pid),
                "instance": retirement.label(),
                "waited_ns": started.elapsed().as_nanos() as u64,
            }),
        );
        if *retirement == Retirement::StopFailed {
            return Err(format!(
                "daemon process {pid} is the recorded instance and its endpoint is gone, \
                 but it could not be stopped"
            ));
        }
    }
    let killed_pid = match retirement {
        Some(Retirement::Stopped { pid }) => Some(pid),
        _ => None,
    };
    if let Some(slot) = &slot {
        slot.reassert();
    }
    spawn_with_slot(
        endpoint,
        crate::core::lifecycle::REASON_REPLACED_UNREACHABLE,
        killed_pid,
        slot,
        replacement,
    )
    .await
    .map_err(|error| {
        format!("daemon process {pid} exists but not accepting connections; replacing it failed: {error}")
    })
}

#[cfg(test)]
#[path = "unreachable_tests.rs"]
mod tests;
