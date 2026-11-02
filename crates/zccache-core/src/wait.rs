//! Blocking waits with mandatory budgets. Product code must use these APIs.

use std::io;
use std::time::{Duration, Instant};

/// A brief external-tool probe.
pub const PROBE: Duration = Duration::from_secs(30);
/// A helper whose own work is bounded.
pub const HELPER_JOIN: Duration = Duration::from_secs(60);
/// An entire user-requested build/download, rather than a brief state access.
pub const WHOLE_COMMAND: Duration = Duration::from_secs(24 * 60 * 60);

/// Receive with a mandatory deadline.
pub fn recv_for<T>(
    rx: &std::sync::mpsc::Receiver<T>,
    bound: Duration,
) -> Result<T, std::sync::mpsc::RecvTimeoutError> {
    rx.recv_timeout(bound)
}

/// Send to a bounded channel without waiting indefinitely. On failure return
/// the unsent value, preserving ownership for retry or cancellation.
pub fn send_for<T>(
    tx: &std::sync::mpsc::SyncSender<T>,
    mut value: T,
    bound: Duration,
) -> Result<(), std::sync::mpsc::TrySendError<T>> {
    let started = Instant::now();
    loop {
        match tx.try_send(value) {
            Ok(()) => return Ok(()),
            Err(std::sync::mpsc::TrySendError::Disconnected(value)) => {
                return Err(std::sync::mpsc::TrySendError::Disconnected(value));
            }
            Err(std::sync::mpsc::TrySendError::Full(returned)) => value = returned,
        }
        if started.elapsed() >= bound {
            return Err(std::sync::mpsc::TrySendError::Full(value));
        }
        std::thread::sleep(Duration::from_millis(1).min(bound.saturating_sub(started.elapsed())));
    }
}

/// Join within a budget. Return ownership on timeout so the caller can stop
/// and retry the worker; do not hide a still-running thread by detaching it.
pub fn join_for<T>(
    handle: std::thread::JoinHandle<T>,
    bound: Duration,
) -> Result<std::thread::Result<T>, std::thread::JoinHandle<T>> {
    let started = Instant::now();
    while !handle.is_finished() {
        if started.elapsed() >= bound {
            return Err(handle);
        }
        std::thread::sleep(Duration::from_millis(1).min(bound.saturating_sub(started.elapsed())));
    }
    Ok(handle.join())
}

/// Wait for a child; on timeout request termination and report the expiry.
/// Reaping gets a separate short budget, so an unkillable child cannot wedge
/// the caller either. Compiler sessions use the daemon's process facade.
pub fn child_for(
    child: &mut std::process::Child,
    bound: Duration,
) -> io::Result<std::process::ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= bound {
            child.kill()?;
            let reap_started = Instant::now();
            while reap_started.elapsed() < Duration::from_secs(5) {
                if child.try_wait()?.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "child wait exceeded its budget",
            ));
        }
        std::thread::sleep(Duration::from_millis(1).min(bound.saturating_sub(started.elapsed())));
    }
}

/// Run a captured probe with bounded output and descendant containment owned
/// by kernal-api, including readers retained by escaped descendants.
pub fn output_for(
    spec: kernal_api::SpawnSpec,
    bound: Duration,
) -> Result<kernal_api::BoundedProcessOutput, kernal_api::BoundedProcessError> {
    kernal_api::run_bounded_command(
        spec.stdin(kernal_api::StreamMode::Null),
        Some(bound),
        1024 * 1024,
    )
}

/// Drive a future with a mandatory deadline. Construct the timeout inside the
/// runtime: constructing it before entering the runtime can panic.
pub fn block_on_for<F: std::future::Future>(
    runtime: &tokio::runtime::Runtime,
    future: F,
    bound: Duration,
) -> Result<F::Output, tokio::time::error::Elapsed> {
    runtime.block_on(async { tokio::time::timeout(bound, future).await })
}

fn file_lock_for<G>(bound: Duration, mut attempt: impl FnMut() -> io::Result<G>) -> io::Result<G> {
    let started = Instant::now();
    loop {
        match attempt() {
            Ok(guard) => return Ok(guard),
            Err(error) if kernal_api::platform::fs::is_lock_conflict(&error) => {
                if started.elapsed() >= bound {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "file lock exceeded its budget",
                    ));
                }
                std::thread::sleep(
                    Duration::from_millis(1).min(bound.saturating_sub(started.elapsed())),
                );
            }
            Err(error) => return Err(error),
        }
    }
}

/// Acquire an advisory exclusive lock with a mandatory deadline.
pub fn lock_exclusive_for(
    file: &std::fs::File,
    bound: Duration,
) -> io::Result<kernal_api::platform::fs::FileLock<'_>> {
    file_lock_for(bound, || kernal_api::platform::fs::try_lock_exclusive(file))
}
/// Acquire an owned shared advisory lock, reopening on every attempt because
/// an unsuccessful owned acquisition consumes its file handle.
pub fn lock_shared_owned_for(
    mut open: impl FnMut() -> io::Result<std::fs::File>,
    bound: Duration,
) -> io::Result<kernal_api::platform::fs::OwnedFileLock> {
    file_lock_for(bound, || {
        kernal_api::platform::fs::try_lock_shared_owned(open()?)
    })
}
/// Acquire an owned exclusive advisory lock within a mandatory deadline.
pub fn lock_exclusive_owned_for(
    mut open: impl FnMut() -> io::Result<std::fs::File>,
    bound: Duration,
) -> io::Result<kernal_api::platform::fs::OwnedFileLock> {
    file_lock_for(bound, || {
        kernal_api::platform::fs::try_lock_exclusive_owned(open()?)
    })
}

/// Platform-neutral spawn description for bounded probe commands.
pub use kernal_api::{BoundedProcessOutput as ProcessOutput, SpawnSpec as CommandSpec};

/// Execute a command description with an explicit timeout and output bound.
pub trait BoundedCommand {
    /// Capture at most one MiB; timeout/output cleanup belongs to kernal-api.
    fn output_for(self, bound: Duration) -> io::Result<ProcessOutput>;
}
impl BoundedCommand for CommandSpec {
    fn output_for(self, bound: Duration) -> io::Result<ProcessOutput> {
        output_for(self, bound).map_err(|error| match error {
            kernal_api::BoundedProcessError::TimedOut => {
                io::Error::new(io::ErrorKind::TimedOut, error)
            }
            other => io::Error::other(other),
        })
    }
}

/// Acquire a shared advisory file lock within a mandatory deadline.
pub fn lock_shared_for(
    file: &std::fs::File,
    bound: Duration,
) -> io::Result<kernal_api::platform::fs::FileLock<'_>> {
    file_lock_for(bound, || kernal_api::platform::fs::try_lock_shared(file))
}
