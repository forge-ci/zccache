//! Owned test subprocesses with mandatory deadlines (#1971).
//!
//! A fixture that pipes a child and then waits can deadlock once the child
//! fills a pipe buffer, and one that stops reading early can wedge the child
//! later. [`TestChild`] drains both pipes continuously for the child's whole
//! lifetime and retains a bounded tail. The child runs in its own contained
//! process group through kernal-api, so termination reaches descendants that
//! stayed in the group, and dropping the owner cleans up on panic too.

use kernal_api::platform::process::{
    spawn_sync, SpawnStdio, SpawnedChild, StdioSource, SyncEnvironment,
};
use std::io::{self, Read};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use zccache_core::blocking_sync::Mutex;

/// Bytes of each stream retained by [`TestChild::spawn`].
pub const DEFAULT_RETENTION: usize = 4 * 1024 * 1024;
/// How long reaping may take after a kill before the owner reports failure.
const REAP_BUDGET: Duration = Duration::from_secs(10);
/// How long readers may take to reach end-of-file once the group is gone. A
/// reader still open after this means a descendant escaped the group.
const DRAIN_BUDGET: Duration = Duration::from_secs(5);
/// Holder-side budget for the short retention critical section.
const STATE_LOCK_BUDGET: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(5);

/// Result of [`TestChild::wait_until`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// The predicate held while the child was running.
    Ready,
    /// The child exited first, with this code.
    Exited(i32),
    /// The budget expired. The child still runs, and its owner still owns it.
    TimedOut,
}

/// Everything captured from a finished child.
#[derive(Clone, Debug)]
pub struct TestOutput {
    /// Exit code, or `None` when the child could not be reaped in budget.
    pub code: Option<i32>,
    /// Retained tail of standard output.
    pub stdout: Vec<u8>,
    /// Retained tail of standard error.
    pub stderr: Vec<u8>,
    /// Total bytes read from standard output, including any discarded.
    pub stdout_total: u64,
    /// Total bytes read from standard error, including any discarded.
    pub stderr_total: u64,
    /// Read failures, which would otherwise look like a quiet child.
    pub reader_errors: Vec<String>,
    /// A pipe was still open after the group died: an escaped descendant holds it.
    pub pipes_retained: bool,
}

impl TestOutput {
    /// Lossy standard output, for assertions and diagnostics.
    #[must_use]
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// Lossy standard error, for assertions and diagnostics.
    #[must_use]
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

#[derive(Default)]
struct DrainState {
    tail: Vec<u8>,
    total: u64,
    error: Option<String>,
    done: bool,
}

/// One continuously drained stream. The thread never blocks the child: it
/// keeps reading after the retention limit is reached and discards the oldest
/// bytes instead.
struct Drain {
    state: Arc<Mutex<DrainState>>,
    limit: usize,
}

impl Drain {
    fn start<R: Read + Send + 'static>(mut source: R, limit: usize) -> io::Result<Self> {
        let state = Arc::new(Mutex::new(DrainState::default()));
        let shared = Arc::clone(&state);
        // A reader blocked on a pipe held by an escaped descendant cannot be
        // interrupted portably, so the thread is detached. It owns only the
        // pipe and this state, and `pipes_retained` reports that it outlived us.
        std::thread::Builder::new()
            .name("test-child-drain".to_string())
            .spawn(move || {
                let mut chunk = vec![0_u8; 64 * 1024];
                loop {
                    match source.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(read) => record(&shared, limit, |tail, total| {
                            tail.extend_from_slice(&chunk[..read]);
                            *total += read as u64;
                        }),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            finish_drain(&shared, Some(error.to_string()));
                            return;
                        }
                    }
                }
                finish_drain(&shared, None);
            })?;
        Ok(Self { state, limit })
    }

    fn is_done(&self) -> bool {
        with_state(&self.state, |state| state.done)
    }

    /// Copy out the retained tail, trimmed to the exact limit.
    fn snapshot(&self) -> DrainState {
        with_state(&self.state, |state| {
            let keep_from = state.tail.len().saturating_sub(self.limit);
            DrainState {
                tail: state.tail[keep_from..].to_vec(),
                total: state.total,
                error: state.error.clone(),
                done: state.done,
            }
        })
    }
}

fn with_state<T>(state: &Mutex<DrainState>, callback: impl FnOnce(&mut DrainState) -> T) -> T {
    // A poisoned or stalled state lock is a broken fixture invariant.
    let mut guard = state
        .lock_for(STATE_LOCK_BUDGET)
        .unwrap_or_else(|error| panic!("test child drain state unavailable: {error}"));
    callback(&mut guard)
}

fn record(state: &Mutex<DrainState>, limit: usize, append: impl FnOnce(&mut Vec<u8>, &mut u64)) {
    with_state(state, |state| {
        append(&mut state.tail, &mut state.total);
        // Trim in batches, so a flooding child stays linear in bytes written.
        if state.tail.len() > limit.saturating_mul(2).max(64 * 1024) {
            let excess = state.tail.len() - limit;
            state.tail.drain(..excess);
        }
    });
}

fn finish_drain(state: &Mutex<DrainState>, error: Option<String>) {
    with_state(state, |state| {
        state.error = error;
        state.done = true;
    });
}

/// Wait for every drain to finish; `false` means one is still held open.
fn drains_finished(drains: &[&Drain], bound: Duration) -> bool {
    let started = Instant::now();
    loop {
        if drains.iter().all(|drain| drain.is_done()) {
            return true;
        }
        if started.elapsed() >= bound {
            return false;
        }
        std::thread::sleep(POLL.min(bound.saturating_sub(started.elapsed())));
    }
}

/// Merge finished drains into the output handed back to the test.
fn assemble(code: Option<i32>, stdout: &Drain, stderr: &Drain, bound: Duration) -> TestOutput {
    let finished = drains_finished(&[stdout, stderr], bound);
    let out = stdout.snapshot();
    let err = stderr.snapshot();
    let reader_errors = [("stdout", out.error), ("stderr", err.error)]
        .into_iter()
        .filter_map(|(stream, error)| error.map(|error| format!("{stream}: {error}")))
        .collect();
    TestOutput {
        code,
        stdout: out.tail,
        stderr: err.tail,
        stdout_total: out.total,
        stderr_total: err.total,
        reader_errors,
        pipes_retained: !finished,
    }
}

/// A subprocess owned by one test. There is deliberately no unbounded wait.
pub struct TestChild {
    child: SpawnedChild,
    stdout: Drain,
    stderr: Drain,
    code: Option<i32>,
}

impl TestChild {
    /// Spawn `command` with null stdin, draining stdout and stderr, retaining
    /// [`DEFAULT_RETENTION`] bytes of each.
    ///
    /// # Errors
    ///
    /// Returns the spawn failure, or the failure to start a drain thread.
    pub fn spawn(command: Command) -> io::Result<Self> {
        Self::spawn_with_retention(command, DEFAULT_RETENTION)
    }

    /// [`TestChild::spawn`] with an explicit per-stream retention limit.
    ///
    /// # Errors
    ///
    /// Returns the spawn failure, or the failure to start a drain thread.
    pub fn spawn_with_retention(mut command: Command, retention: usize) -> io::Result<Self> {
        let mut child = spawn_sync(
            &mut command,
            SpawnStdio {
                stdout: StdioSource::Pipe,
                stderr: StdioSource::Pipe,
                ..SpawnStdio::default()
            },
            SyncEnvironment::Inherit,
        )?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("contained child returned no stdout pipe"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("contained child returned no stderr pipe"))?;
        Ok(Self {
            stdout: Drain::start(stdout, retention)?,
            stderr: Drain::start(stderr, retention)?,
            child,
            code: None,
        })
    }

    /// The child's process id.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Return the exit code if the child has finished, without blocking.
    ///
    /// # Errors
    ///
    /// Returns the operating system's wait failure.
    pub fn try_wait(&mut self) -> io::Result<Option<i32>> {
        if self.code.is_none() {
            self.code = self.child.try_wait()?;
        }
        Ok(self.code)
    }

    /// Poll `ready` until it holds, the child exits, or `bound` expires. The
    /// child keeps running on expiry, so the test can still read its output.
    ///
    /// # Errors
    ///
    /// Returns the operating system's wait failure.
    pub fn wait_until(
        &mut self,
        bound: Duration,
        mut ready: impl FnMut() -> bool,
    ) -> io::Result<Readiness> {
        let started = Instant::now();
        loop {
            if ready() {
                return Ok(Readiness::Ready);
            }
            if let Some(code) = self.try_wait()? {
                return Ok(Readiness::Exited(code));
            }
            if started.elapsed() >= bound {
                return Ok(Readiness::TimedOut);
            }
            std::thread::sleep(POLL.min(bound.saturating_sub(started.elapsed())));
        }
    }

    /// Wait up to `bound` for the child to exit. On expiry the process group
    /// is terminated and reaped, and [`io::ErrorKind::TimedOut`] is returned.
    ///
    /// # Errors
    ///
    /// Returns `TimedOut` on expiry, or the operating system's wait failure.
    pub fn wait_for(&mut self, bound: Duration) -> io::Result<i32> {
        match self.wait_until(bound, || false)? {
            Readiness::Exited(code) => Ok(code),
            Readiness::Ready | Readiness::TimedOut => {
                self.terminate()?;
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("test child exceeded its {bound:?} budget"),
                ))
            }
        }
    }

    /// Terminate the process group and reap the child within a fixed budget.
    /// Calling this after the child has exited still signals the group, so
    /// descendants of an exited leader are cleaned up.
    ///
    /// # Errors
    ///
    /// Returns `TimedOut` if the child could not be reaped in budget, or the
    /// operating system's kill or wait failure.
    pub fn terminate(&mut self) -> io::Result<Option<i32>> {
        self.child.kill()?;
        match self.wait_until(REAP_BUDGET, || false)? {
            Readiness::Exited(code) => Ok(Some(code)),
            Readiness::Ready | Readiness::TimedOut => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("test child was not reaped within {REAP_BUDGET:?}"),
            )),
        }
    }

    /// Wait up to `bound` for exit, then collect all captured output. On
    /// expiry the group is terminated and the error carries the captured tail.
    ///
    /// # Errors
    ///
    /// Returns `TimedOut` on expiry, or the operating system's wait failure.
    pub fn finish(mut self, bound: Duration) -> io::Result<TestOutput> {
        match self.wait_for(bound) {
            Ok(_) => Ok(self.collect()),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                let output = self.collect();
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "{error}\nstdout:\n{}\nstderr:\n{}",
                        output.stdout_text(),
                        output.stderr_text()
                    ),
                ))
            }
            Err(error) => Err(error),
        }
    }

    /// Terminate the group if it is running, and collect all captured output.
    /// A reap failure is reported as `code: None`, never as a hang.
    #[must_use]
    pub fn collect(mut self) -> TestOutput {
        // Signal even after a leader exit: a descendant left in the group
        // would otherwise keep the pipes open for the rest of the drain budget.
        let _ = self.child.kill();
        if self.code.is_none() {
            let _ = self.wait_until(REAP_BUDGET, || false);
        }
        assemble(self.code, &self.stdout, &self.stderr, DRAIN_BUDGET)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const TEST_BOUND: Duration = Duration::from_millis(200);

    struct FailingRead;
    impl Read for FailingRead {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("planted read failure"))
        }
    }

    /// A reader that never reaches end-of-file, as with an escaped descendant.
    struct NeverEof(std::sync::mpsc::Receiver<()>);
    impl Read for NeverEof {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            let _ = zccache_core::wait::recv_for(&self.0, Duration::from_secs(60));
            Ok(0)
        }
    }

    fn finished(bytes: &[u8], limit: usize) -> Drain {
        let drain = Drain::start(io::Cursor::new(bytes.to_vec()), limit).unwrap();
        assert!(drains_finished(&[&drain], Duration::from_secs(10)));
        drain
    }

    #[test]
    fn reader_errors_are_reported_rather_than_read_as_quiet_output() {
        let failing = Drain::start(FailingRead, 16).unwrap();
        let empty = finished(b"", 16);
        let output = assemble(Some(0), &failing, &empty, Duration::from_secs(10));
        assert_eq!(output.reader_errors.len(), 1, "{:?}", output.reader_errors);
        assert!(output.reader_errors[0].starts_with("stdout: "));
        assert!(output.reader_errors[0].contains("planted read failure"));
        assert!(!output.pipes_retained);
    }

    #[test]
    fn a_pipe_held_open_is_bounded_and_reported_as_retained() {
        let (keep_open, receiver) = std::sync::mpsc::channel();
        let held = Drain::start(NeverEof(receiver), 16).unwrap();
        let empty = finished(b"", 16);
        let started = Instant::now();
        let output = assemble(Some(0), &held, &empty, TEST_BOUND);
        assert!(output.pipes_retained);
        assert!(started.elapsed() < Duration::from_secs(30));
        drop(keep_open);
    }

    #[test]
    fn retention_keeps_the_newest_bytes_and_counts_everything() {
        let bytes: Vec<u8> = (0..=255_u8).cycle().take(300_000).collect();
        let snapshot = finished(&bytes, 1024).snapshot();
        assert_eq!(snapshot.total, 300_000);
        assert_eq!(snapshot.tail, bytes[bytes.len() - 1024..]);
    }
}
