//! Timeout-only synchronous locks. Async callers must use [`super::sync::State`].
//!
//! Poisoning retains std's semantics: callers deciding to fail on poison must
//! not silently recover partially mutated cache metadata. Acquisition expiry
//! fails loudly at the caller, rather than leaving an executor wedged.

use super::sync_watchdog::Ticket;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, LockResult, PoisonError, TryLockError, TryLockResult};
use std::time::{Duration, Instant};

fn tracked<G, W>(
    result: LockResult<G>,
    mut ticket: Ticket,
    limit: Duration,
    wrap: impl FnOnce(G, Ticket) -> W,
) -> LockResult<W> {
    let (inner, poisoned) = match result {
        Ok(inner) => (inner, false),
        Err(error) => (error.into_inner(), true),
    };
    ticket.holding(limit);
    let guard = wrap(inner, ticket);
    if poisoned {
        Err(PoisonError::new(guard))
    } else {
        Ok(guard)
    }
}

fn tracked_try<G, W>(
    result: TryLockResult<G>,
    ticket: Ticket,
    limit: Duration,
    wrap: impl FnOnce(G, Ticket) -> W,
) -> TryLockResult<W> {
    let result = match result {
        Ok(inner) => Ok(inner),
        Err(TryLockError::Poisoned(error)) => Err(error),
        Err(TryLockError::WouldBlock) => return Err(TryLockError::WouldBlock),
    };
    tracked(result, ticket, limit, wrap).map_err(TryLockError::Poisoned)
}

/// Native mutex ownership, registered until the underlying guard releases.
#[derive(Debug)]
pub struct MutexGuard<'a, T: ?Sized> {
    inner: ManuallyDrop<std::sync::MutexGuard<'a, T>>,
    wakeup: &'a Wakeup,
    _ticket: Ticket,
}
/// Native shared ownership with holder diagnostics.
#[derive(Debug)]
pub struct RwLockReadGuard<'a, T: ?Sized> {
    inner: ManuallyDrop<std::sync::RwLockReadGuard<'a, T>>,
    wakeup: &'a Wakeup,
    _ticket: Ticket,
}
/// Native exclusive ownership with holder diagnostics.
#[derive(Debug)]
pub struct RwLockWriteGuard<'a, T: ?Sized> {
    inner: ManuallyDrop<std::sync::RwLockWriteGuard<'a, T>>,
    wakeup: &'a Wakeup,
    _ticket: Ticket,
}

macro_rules! deref_guard {
    ($guard:ident) => {
        impl<T: ?Sized> std::ops::Deref for $guard<'_, T> {
            type Target = T;
            fn deref(&self) -> &T {
                &self.inner
            }
        }
    };
}
macro_rules! release_guard {
    ($guard:ident) => {
        impl<T: ?Sized> Drop for $guard<'_, T> {
            fn drop(&mut self) {
                // SAFETY: `inner` is dropped exactly once, here, and never
                // touched again. Releasing it before waking keeps the handoff
                // immediate for a waiter that re-checks under `park`.
                unsafe { ManuallyDrop::drop(&mut self.inner) };
                self.wakeup.notify();
            }
        }
    };
}
release_guard!(MutexGuard);
release_guard!(RwLockReadGuard);
release_guard!(RwLockWriteGuard);
deref_guard!(MutexGuard);
deref_guard!(RwLockReadGuard);
deref_guard!(RwLockWriteGuard);
impl<T: ?Sized> std::ops::DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}
impl<T: ?Sized> std::ops::DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}
impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// Release notification for contended bounded waiters.
///
/// std has no timed lock, so a contended waiter parks on this condvar and
/// every guard release wakes it. #1972 replaced std's blocking locks with a
/// 50 us -> 5 ms sleep poll; parallel cache-hit materializers contending on
/// the process-wide spawn/materialize lock then slept long after each release
/// and C++ multi-file warm hits regressed ~2x. Waiters re-check the lock while
/// holding `park`, and a releaser takes `park` before notifying, so a release
/// cannot slip between a failed attempt and the wait.
#[derive(Debug)]
pub struct Wakeup {
    waiters: AtomicUsize,
    park: std::sync::Mutex<()>,
    released: Condvar,
}

impl Wakeup {
    const fn new() -> Self {
        Self {
            waiters: AtomicUsize::new(0),
            park: std::sync::Mutex::new(()),
            released: Condvar::new(),
        }
    }
    /// Wake every parked waiter; free when nobody waits.
    fn notify(&self) {
        if self.waiters.load(Ordering::SeqCst) > 0 {
            drop(self.park.lock().unwrap_or_else(PoisonError::into_inner));
            self.released.notify_all();
        }
    }
}

struct Parked<'a>(&'a Wakeup);
impl Drop for Parked<'_> {
    fn drop(&mut self) {
        self.0.waiters.fetch_sub(1, Ordering::SeqCst);
    }
}

#[track_caller]
fn acquire_for<G>(
    bound: Duration,
    wakeup: &Wakeup,
    mut attempt: impl FnMut() -> TryLockResult<G>,
) -> LockResult<G> {
    match attempt() {
        Ok(guard) => return Ok(guard),
        Err(TryLockError::Poisoned(error)) => return Err(error),
        Err(TryLockError::WouldBlock) => {}
    }
    let started = Instant::now();
    wakeup.waiters.fetch_add(1, Ordering::SeqCst);
    let _parked = Parked(wakeup);
    let mut park = wakeup.park.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        match attempt() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(error)) => return Err(error),
            Err(TryLockError::WouldBlock) => {}
        }
        let remaining = bound.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            drop(park);
            super::sync::synchronization_failed(
                std::panic::Location::caller(),
                super::sync::LockTimeout(bound),
            );
        }
        park = wakeup
            .released
            .wait_timeout(park, remaining)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

/// A synchronous mutex whose blocking acquisition requires a budget.
#[derive(Debug)]
pub struct Mutex<T: ?Sized> {
    hold_limit: Duration,
    wakeup: Wakeup,
    inner: std::sync::Mutex<T>,
}

impl<T> Mutex<T> {
    /// Create a mutex.
    pub const fn new(value: T) -> Self {
        Self::with_hold_limit(value, super::sync::STATE_TIMEOUT)
    }
    /// Create a critical section with an explicit justified hold budget.
    pub const fn with_hold_limit(value: T, hold_limit: Duration) -> Self {
        Self {
            hold_limit,
            wakeup: Wakeup::new(),
            inner: std::sync::Mutex::new(value),
        }
    }
    /// Consume the mutex, preserving poisoning.
    pub fn into_inner(self) -> LockResult<T> {
        self.inner.into_inner()
    }
}
impl<T: ?Sized> Mutex<T> {
    /// Acquire within `bound`; expiry diagnoses a broken synchronization invariant.
    #[track_caller]
    pub fn lock_for(&self, bound: Duration) -> LockResult<MutexGuard<'_, T>> {
        let ticket = Ticket::waiting(self, "Mutex", std::panic::Location::caller());
        tracked(
            acquire_for(bound, &self.wakeup, || self.inner.try_lock()),
            ticket,
            self.hold_limit,
            |inner, ticket| MutexGuard {
                inner: ManuallyDrop::new(inner),
                wakeup: &self.wakeup,
                _ticket: ticket,
            },
        )
    }
    /// Attempt without blocking.
    #[track_caller]
    pub fn try_lock(&self) -> TryLockResult<MutexGuard<'_, T>> {
        let ticket = Ticket::waiting(self, "Mutex", std::panic::Location::caller());
        tracked_try(
            self.inner.try_lock(),
            ticket,
            self.hold_limit,
            |inner, ticket| MutexGuard {
                inner: ManuallyDrop::new(inner),
                wakeup: &self.wakeup,
                _ticket: ticket,
            },
        )
    }
    /// Access exclusively borrowed data without locking.
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        self.inner.get_mut()
    }
    /// Whether a holder panicked.
    pub fn is_poisoned(&self) -> bool {
        self.inner.is_poisoned()
    }
}

/// A synchronous reader/writer lock with mandatory acquisition budgets.
#[derive(Debug)]
pub struct RwLock<T: ?Sized> {
    waiting_writers: AtomicUsize,
    wakeup: Wakeup,
    read_hold_limit: Duration,
    write_hold_limit: Duration,
    inner: std::sync::RwLock<T>,
}
impl<T> RwLock<T> {
    /// Create an unlocked value.
    pub const fn new(value: T) -> Self {
        Self::with_hold_limits(
            value,
            super::sync::STATE_TIMEOUT,
            super::sync::STATE_TIMEOUT,
        )
    }
    /// Give shared native spawn and exclusive artifact copies separate hold budgets.
    pub const fn with_hold_limits(
        value: T,
        read_hold_limit: Duration,
        write_hold_limit: Duration,
    ) -> Self {
        Self {
            waiting_writers: AtomicUsize::new(0),
            wakeup: Wakeup::new(),
            read_hold_limit,
            write_hold_limit,
            inner: std::sync::RwLock::new(value),
        }
    }
    /// Consume the lock, preserving poisoning.
    pub fn into_inner(self) -> LockResult<T> {
        self.inner.into_inner()
    }
}
impl<T: ?Sized> RwLock<T> {
    /// Acquire shared access within a mandatory budget.
    #[track_caller]
    pub fn read_for(&self, bound: Duration) -> LockResult<RwLockReadGuard<'_, T>> {
        let ticket = Ticket::waiting(self, "RwLock(read)", std::panic::Location::caller());
        tracked(
            acquire_for(bound, &self.wakeup, || {
                if self.waiting_writers.load(Ordering::Acquire) > 0 {
                    Err(TryLockError::WouldBlock)
                } else {
                    self.inner.try_read()
                }
            }),
            ticket,
            self.read_hold_limit,
            |inner, ticket| RwLockReadGuard {
                inner: ManuallyDrop::new(inner),
                wakeup: &self.wakeup,
                _ticket: ticket,
            },
        )
    }
    /// Acquire exclusive access within a mandatory budget.
    #[track_caller]
    pub fn write_for(&self, bound: Duration) -> LockResult<RwLockWriteGuard<'_, T>> {
        // Polling try_write alone never joins std's native writer queue:
        // continuous readers could starve the writer forever. Publish writer
        // intent before polling so later readers preserve writer preference.
        self.waiting_writers.fetch_add(1, Ordering::AcqRel);
        let _waiting = WaitingWriter(self);
        let ticket = Ticket::waiting(self, "RwLock(write)", std::panic::Location::caller());
        tracked(
            acquire_for(bound, &self.wakeup, || self.inner.try_write()),
            ticket,
            self.write_hold_limit,
            |inner, ticket| RwLockWriteGuard {
                inner: ManuallyDrop::new(inner),
                wakeup: &self.wakeup,
                _ticket: ticket,
            },
        )
    }
    /// Attempt shared access without blocking.
    #[track_caller]
    pub fn try_read(&self) -> TryLockResult<RwLockReadGuard<'_, T>> {
        if self.waiting_writers.load(Ordering::Acquire) > 0 {
            Err(TryLockError::WouldBlock)
        } else {
            let ticket = Ticket::waiting(self, "RwLock(read)", std::panic::Location::caller());
            tracked_try(
                self.inner.try_read(),
                ticket,
                self.read_hold_limit,
                |inner, ticket| RwLockReadGuard {
                    inner: ManuallyDrop::new(inner),
                    wakeup: &self.wakeup,
                    _ticket: ticket,
                },
            )
        }
    }
    /// Attempt exclusive access without blocking.
    #[track_caller]
    pub fn try_write(&self) -> TryLockResult<RwLockWriteGuard<'_, T>> {
        let ticket = Ticket::waiting(self, "RwLock(write)", std::panic::Location::caller());
        tracked_try(
            self.inner.try_write(),
            ticket,
            self.write_hold_limit,
            |inner, ticket| RwLockWriteGuard {
                inner: ManuallyDrop::new(inner),
                wakeup: &self.wakeup,
                _ticket: ticket,
            },
        )
    }
    /// Access exclusively borrowed data without locking.
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        self.inner.get_mut()
    }
}

struct WaitingWriter<'a, T: ?Sized>(&'a RwLock<T>);
impl<T: ?Sized> Drop for WaitingWriter<'_, T> {
    fn drop(&mut self) {
        self.0.waiting_writers.fetch_sub(1, Ordering::AcqRel);
        // Readers held back only by writer intent re-check now.
        self.0.wakeup.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_writer_prevents_later_readers_from_bypassing_it() {
        let lock = RwLock::new(0);
        std::thread::scope(|scope| {
            let reader = lock.read_for(Duration::from_secs(1)).unwrap();
            let writer = scope.spawn(|| {
                *lock.write_for(Duration::from_secs(5)).unwrap() = 1;
            });
            let started = Instant::now();
            while lock.waiting_writers.load(Ordering::Acquire) == 0
                && started.elapsed() < Duration::from_secs(1)
            {
                std::thread::yield_now();
            }
            assert_eq!(lock.waiting_writers.load(Ordering::Acquire), 1);
            assert!(matches!(lock.try_read(), Err(TryLockError::WouldBlock)));
            drop(reader);
            writer.join().unwrap();
        });
        assert_eq!(*lock.read_for(Duration::from_secs(1)).unwrap(), 1);
    }
}

#[cfg(test)]
mod handoff_latency_tests {
    use super::*;
    use std::sync::Arc;

    const ROUNDS: usize = 31;
    /// Long enough for the waiter to reach its park or backoff sleep.
    const HOLD: Duration = Duration::from_millis(8);
    /// Median latency the lock may add over an ideal condvar wake. A timer
    /// poll with a sleep cap of `P` adds about `P / 2`, so the old 5 ms cap
    /// costs 2.5 ms here and anything polling at 2 ms or slower fails.
    const MAX_EXCESS: Duration = Duration::from_millis(1);

    /// One release-to-wake sample through a bare condvar, the fastest wake the
    /// same scheduler can deliver at this moment.
    fn control_round() -> Duration {
        let slot = Arc::new((std::sync::Mutex::new(None::<Instant>), Condvar::new()));
        let waiter = {
            let slot = Arc::clone(&slot);
            std::thread::spawn(move || {
                let mut released = slot.0.lock().unwrap();
                while released.is_none() {
                    released = slot.1.wait(released).unwrap();
                }
                released.expect("holder stamps release").elapsed()
            })
        };
        std::thread::sleep(HOLD);
        *slot.0.lock().unwrap() = Some(Instant::now());
        slot.1.notify_all();
        waiter.join().unwrap()
    }

    fn mutex_round() -> Duration {
        let mutex = Arc::new(Mutex::new(None::<Instant>));
        let mut guard = mutex.lock_for(Duration::from_secs(5)).unwrap();
        let waiter = {
            let mutex = Arc::clone(&mutex);
            std::thread::spawn(move || {
                let released = mutex.lock_for(Duration::from_secs(5)).unwrap();
                released.expect("holder stamps release").elapsed()
            })
        };
        std::thread::sleep(HOLD);
        *guard = Some(Instant::now());
        drop(guard);
        waiter.join().unwrap()
    }

    fn rwlock_writer_round() -> Duration {
        let lock = Arc::new(RwLock::new(None::<Instant>));
        let mut guard = lock.write_for(Duration::from_secs(5)).unwrap();
        let waiter = {
            let lock = Arc::clone(&lock);
            std::thread::spawn(move || {
                let released = lock.write_for(Duration::from_secs(5)).unwrap();
                released.expect("holder stamps release").elapsed()
            })
        };
        std::thread::sleep(HOLD);
        *guard = Some(Instant::now());
        drop(guard);
        waiter.join().unwrap()
    }

    fn median(samples: &mut [Duration]) -> Duration {
        samples.sort_unstable();
        samples[samples.len() / 2]
    }

    /// Host load delays any wake, so a single worst sample or a fixed absolute
    /// bound measures the scheduler (#2059). Interleaving lock and control
    /// rounds cancels that delay, and the median ignores individual stalls,
    /// while a sleep-timer wake shifts the whole lock distribution.
    fn assert_handoff_tracks_control(label: &str, lock_round: fn() -> Duration) {
        let mut lock = Vec::with_capacity(ROUNDS);
        let mut control = Vec::with_capacity(ROUNDS);
        for _ in 0..ROUNDS {
            lock.push(lock_round());
            control.push(control_round());
        }
        let lock = median(&mut lock);
        let control = median(&mut control);
        eprintln!(
            "{label} handoff: median {} ns, control median {} ns",
            lock.as_nanos(),
            control.as_nanos()
        );
        assert!(
            lock.saturating_sub(control) < MAX_EXCESS,
            "contended {label} handoff median {} ns exceeds control median {} ns by the \
             {} ns budget: waiters are sleeping through releases (#1972)",
            lock.as_nanos(),
            control.as_nanos(),
            MAX_EXCESS.as_nanos()
        );
    }

    /// Perf guard for the #1972 regression: a contended waiter must observe a
    /// release promptly instead of sleeping through a multi-ms backoff step.
    #[test]
    fn contended_mutex_handoff_is_sub_millisecond() {
        assert_handoff_tracks_control("mutex", mutex_round);
    }

    /// The regression shape: parallel materializers each take the
    /// process-wide spawn/materialize lock exclusively. A parked writer must
    /// wake on release rather than on a sleep timer.
    #[test]
    fn contended_rwlock_writer_handoff_is_sub_millisecond() {
        assert_handoff_tracks_control("rwlock writer", rwlock_writer_round);
    }

    /// A reader held back only by writer intent must be woken when that
    /// writer finishes; a lost wake-up would stall it until its bound.
    #[test]
    fn reader_parked_behind_writer_intent_wakes_after_writer() {
        let lock = Arc::new(RwLock::new(0));
        let first_reader = lock.read_for(Duration::from_secs(5)).unwrap();
        let writer = {
            let lock = Arc::clone(&lock);
            std::thread::spawn(move || *lock.write_for(Duration::from_secs(5)).unwrap() = 1)
        };
        while lock.waiting_writers.load(Ordering::Acquire) == 0 {
            std::thread::yield_now();
        }
        let reader = {
            let lock = Arc::clone(&lock);
            std::thread::spawn(move || {
                let started = Instant::now();
                let value = *lock.read_for(Duration::from_secs(5)).unwrap();
                (value, started.elapsed())
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        drop(first_reader);
        writer.join().unwrap();
        let (value, waited) = reader.join().unwrap();
        assert_eq!(value, 1, "writer preference: reader runs after the writer");
        assert!(
            waited < Duration::from_secs(1),
            "reader stalled {} ns after the writer released",
            waited.as_nanos()
        );
    }

    /// Many exclusive contenders must all make progress without lost wake-ups.
    #[test]
    fn many_contended_writers_all_complete_promptly() {
        const THREADS: usize = 8;
        const ROUNDS: usize = 200;
        let lock = Arc::new(RwLock::new(0usize));
        let started = Instant::now();
        let workers: Vec<_> = (0..THREADS)
            .map(|_| {
                let lock = Arc::clone(&lock);
                std::thread::spawn(move || {
                    for _ in 0..ROUNDS {
                        *lock.write_for(Duration::from_secs(5)).unwrap() += 1;
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(
            *lock.read_for(Duration::from_secs(1)).unwrap(),
            THREADS * ROUNDS
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "contended writers took {} ns",
            started.elapsed().as_nanos()
        );
    }
}
