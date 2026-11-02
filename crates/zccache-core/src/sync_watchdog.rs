//! Private bookkeeping for short critical sections, independent of async runtimes.
use std::io::Write;
use std::panic::Location;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Waiting,
    Holding,
}

#[derive(Clone, Debug)]
struct Entry {
    id: u64,
    owner: usize,
    kind: &'static str,
    phase: Phase,
    site: &'static Location<'static>,
    thread: String,
    since: Instant,
    limit: Duration,
    reported: bool,
}

fn registry() -> &'static Mutex<Vec<Entry>> {
    static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
    &REGISTRY
}

fn with_entries<R>(operation: impl FnOnce(&mut Vec<Entry>) -> R) -> R {
    // The only raw bookkeeping mutex. No user code, I/O or waits run under it.
    operation(&mut registry().lock().unwrap_or_else(PoisonError::into_inner))
}

fn thread_name() -> String {
    let thread = std::thread::current();
    thread
        .name()
        .map_or_else(|| format!("{:?}", thread.id()), str::to_owned)
}

pub(crate) fn acquisition_failed(site: &Location<'_>, error: super::sync::LockTimeout) -> ! {
    let entries = with_entries(|entries| entries.clone());
    let text = format!(
        "bounded synchronization failed at {site}: {error}\n{}",
        report(&entries, Instant::now(), false)
    );
    abort_after_report(reporter(), text)
}

struct Diagnostic {
    text: String,
    complete: Option<SyncSender<()>>,
}

/// Exactly one process-lifetime writer and one queued report. A stuck sink
/// cannot accumulate writers or prevent the monitor from enforcing hold limits.
fn reporter() -> &'static SyncSender<Diagnostic> {
    static REPORTER: OnceLock<SyncSender<Diagnostic>> = OnceLock::new();
    REPORTER.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel::<Diagnostic>(1);
        let _worker = std::thread::Builder::new()
            .name("zccache-lock-diagnostics".into())
            .spawn(move || loop {
                let diagnostic = match receiver.recv_timeout(Duration::from_secs(1)) {
                    Ok(diagnostic) => diagnostic,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                };
                // Direct I/O survives libtest capture. Only this dedicated
                // writer can block on the sink; never the monitor/abort path.
                let _ = std::io::stderr().write_all(diagnostic.text.as_bytes());
                if let Some(complete) = diagnostic.complete {
                    let _ = complete.try_send(());
                }
            })
            .unwrap_or_else(|_| std::process::abort());
        sender
    })
}

fn abort_after_report(sender: &SyncSender<Diagnostic>, text: String) -> ! {
    let (complete, receiver) = mpsc::sync_channel(1);
    if sender
        .try_send(Diagnostic {
            text,
            complete: Some(complete),
        })
        .is_ok()
    {
        // Healthy sinks get a chance to finish the complete context. A full
        // pipe, logging lock, broken worker or full queue cannot defer abort.
        let _ = receiver.recv_timeout(Duration::from_millis(250));
    }
    std::process::abort()
}

#[cfg(test)]
pub(crate) fn registered<T: ?Sized>(owner: &T) -> usize {
    let owner = std::ptr::from_ref(owner).cast::<()>() as usize;
    with_entries(|entries| entries.iter().filter(|entry| entry.owner == owner).count())
}

/// Owns one waiting/holding registration; cancellation and unwinding remove it.
#[derive(Debug)]
pub(crate) struct Ticket(u64);

impl Ticket {
    pub(crate) fn waiting<T: ?Sized>(
        owner: &T,
        kind: &'static str,
        site: &'static Location<'static>,
    ) -> Self {
        start();
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let entry = Entry {
            id,
            owner: std::ptr::from_ref(owner).cast::<()>() as usize,
            kind,
            phase: Phase::Waiting,
            site,
            thread: thread_name(),
            since: Instant::now(),
            limit: super::sync::STATE_TIMEOUT,
            reported: false,
        };
        with_entries(|entries| entries.push(entry));
        Self(id)
    }

    pub(crate) fn holding(&mut self, limit: Duration) {
        let thread = thread_name();
        with_entries(|entries| {
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == self.0) {
                entry.phase = Phase::Holding;
                entry.since = Instant::now();
                entry.limit = limit;
                entry.reported = false;
                entry.thread = thread;
            }
        });
    }
}

#[cfg(test)]
#[path = "sync_watchdog_registry_tests.rs"]
mod tests;

impl Drop for Ticket {
    fn drop(&mut self) {
        with_entries(|entries| {
            if let Some(index) = entries.iter().position(|entry| entry.id == self.0) {
                entries.swap_remove(index);
            }
        });
    }
}

/// Formatting and stderr writes run outside the registry mutex.
fn report(entries: &[Entry], now: Instant, abort: bool) -> String {
    use std::fmt::Write;
    let phase = if abort { "hold" } else { "wait" };
    let mut text = format!("zccache: lock {phase} exceeded its budget. Registered sites:\n");
    for entry in entries {
        let _ = writeln!(
            text,
            "  {:?} {} owner={:#x} age_ns={} budget_ns={} at {} (thread {})",
            entry.phase,
            entry.kind,
            entry.owner,
            now.saturating_duration_since(entry.since).as_nanos(),
            entry.limit.as_nanos(),
            entry.site,
            entry.thread,
        );
    }
    text
}

fn decision(entries: &mut [Entry], now: Instant) -> Option<(bool, Vec<Entry>)> {
    let mut abort = false;
    let mut notify = false;
    for entry in entries.iter_mut() {
        if now.saturating_duration_since(entry.since) < entry.limit {
            continue;
        }
        if entry.phase == Phase::Holding {
            abort = true;
        } else if !entry.reported {
            entry.reported = true;
            notify = true;
        }
    }
    (abort || notify).then(|| (abort, entries.to_vec()))
}

fn start() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let sender = reporter().clone();
        if let Err(error) = std::thread::Builder::new()
            .name("zccache-lock-watchdog".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(100));
                let now = Instant::now();
                let observation = with_entries(|entries| decision(entries, now));
                if let Some((abort, entries)) = observation {
                    let text = report(&entries, now, abort);
                    if abort {
                        abort_after_report(&sender, text);
                    }
                    let _ = sender.try_send(Diagnostic {
                        text,
                        complete: None,
                    });
                }
            })
        {
            // A missing backstop must not silently downgrade the safety contract.
            abort_after_report(
                reporter(),
                format!("zccache: lock watchdog could not start: {error}\n"),
            );
        }
    });
}
