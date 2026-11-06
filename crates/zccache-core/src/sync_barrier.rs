//! Unit-only publication/admission barriers: permits never expose user data.

use super::sync::{synchronization_failed, LockTimeout};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Shared/exclusive operation exclusion, separate from protected state.
#[derive(Debug, Clone)]
pub struct Barrier {
    inner: Arc<tokio::sync::RwLock<()>>,
}
/// Shared operation permit; safe to carry through async work.
pub struct ReadPermit {
    _guard: tokio::sync::OwnedRwLockReadGuard<()>,
}
/// Exclusive operation permit; safe to carry through async work.
pub struct WritePermit {
    _guard: tokio::sync::OwnedRwLockWriteGuard<()>,
}

impl Default for Barrier {
    fn default() -> Self {
        Self::new()
    }
}
impl Barrier {
    /// Create an unlocked barrier.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(tokio::sync::RwLock::new(())),
        }
    }
    /// Acquire shared ownership within a mandatory budget.
    #[track_caller]
    pub fn read_for(
        &self,
        bound: Duration,
    ) -> impl std::future::Future<Output = ReadPermit> + use<'_> {
        let site = std::panic::Location::caller();
        async move {
            match tokio::time::timeout(bound, Arc::clone(&self.inner).read_owned()).await {
                Ok(guard) => ReadPermit { _guard: guard },
                Err(_) => synchronization_failed(site, LockTimeout(bound)),
            }
        }
    }
    /// Try exclusive ownership; timeout leaves the queue without a permit.
    pub async fn try_write_for(&self, bound: Duration) -> Result<WritePermit, LockTimeout> {
        tokio::time::timeout(bound, Arc::clone(&self.inner).write_owned())
            .await
            .map(|guard| WritePermit { _guard: guard })
            .map_err(|_| LockTimeout(bound))
    }
    /// Acquire essential exclusive ownership with an explicit budget.
    #[track_caller]
    pub fn write_for(
        &self,
        bound: Duration,
    ) -> impl std::future::Future<Output = WritePermit> + use<'_> {
        let site = std::panic::Location::caller();
        async move {
            match self.try_write_for(bound).await {
                Ok(permit) => permit,
                Err(error) => synchronization_failed(site, error),
            }
        }
    }
    /// Try shared ownership without blocking.
    pub fn try_read_owned(&self) -> Result<ReadPermit, tokio::sync::TryLockError> {
        Arc::clone(&self.inner)
            .try_read_owned()
            .map(|guard| ReadPermit { _guard: guard })
    }
    /// Try exclusive ownership without blocking.
    pub fn try_write(&self) -> Result<WritePermit, tokio::sync::TryLockError> {
        Arc::clone(&self.inner)
            .try_write_owned()
            .map(|guard| WritePermit { _guard: guard })
    }
    /// Shared ownership for blocking workers, with a mandatory budget.
    #[track_caller]
    pub fn blocking_read_for(&self, bound: Duration) -> ReadPermit {
        ReadPermit {
            _guard: blocking_for(Arc::clone(&self.inner).read_owned(), bound),
        }
    }
    /// Exclusive ownership for blocking workers, retaining FIFO queue position.
    #[track_caller]
    pub fn blocking_write_for(&self, bound: Duration) -> WritePermit {
        WritePermit {
            _guard: blocking_for(Arc::clone(&self.inner).write_owned(), bound),
        }
    }
}

struct ThreadWake(std::thread::Thread);
impl std::task::Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Poll one acquisition for its entire wait. Recreating try-lock attempts
/// loses Tokio's queue position and lets later readers starve a writer.
/// The semaphore acquisition future needs no runtime or timer; its waker
/// unparks this blocking worker, and park_timeout supplies the deadline.
#[track_caller]
pub(super) fn blocking_for<F: std::future::Future>(future: F, bound: Duration) -> F::Output {
    let started = Instant::now();
    let waker = std::task::Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        let remaining = bound.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            synchronization_failed(std::panic::Location::caller(), LockTimeout(bound));
        }
        std::thread::park_timeout(remaining);
    }
}
