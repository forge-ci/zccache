//! Publication reads, short commits, and whole-checkpoint drain ownership.
//!
//! Admission permits deliberately cover complete I/O operations. Publication
//! commit permits instead cover live-name retirement and metadata changes;
//! their distinct type lets Dylint reject slow cleanup before release.
use super::sync_barrier::{Barrier, ReadPermit, WritePermit};
use std::time::Duration;

/// Publication exclusion whose destructive commits have a distinct capability.
#[derive(Debug, Clone, Default)]
pub struct PublicationBarrier {
    inner: Barrier,
}

/// Shared publication/materialization ownership, distinct from admission.
#[must_use = "retain publication ownership through the publication or hit"]
pub struct PublicationReadPermit {
    _permit: ReadPermit,
}

/// Short blocking publication commit. Release before sweeping detached names.
#[must_use = "retain publication exclusion through the live-name commit"]
pub struct PublicationCommitPermit {
    _permit: WritePermit,
}

/// Whole-checkpoint or shutdown ownership, distinct from a short commit.
/// Only the bounded service drain owners may acquire this capability.
#[must_use = "retain drain ownership through the final checkpoint"]
pub struct PublicationDrainPermit {
    _permit: WritePermit,
}

impl PublicationBarrier {
    /// Create an unlocked publication coordinator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire a shared publication or hit-materialization lease.
    #[track_caller]
    pub fn read_for(
        &self,
        bound: Duration,
    ) -> impl std::future::Future<Output = PublicationReadPermit> + use<'_> {
        let waiting = self.inner.read_for(bound);
        async move {
            PublicationReadPermit {
                _permit: waiting.await,
            }
        }
    }

    /// Acquire a shared lease on the blocking lane with a mandatory budget.
    #[track_caller]
    pub fn blocking_read_for(&self, bound: Duration) -> PublicationReadPermit {
        PublicationReadPermit {
            _permit: self.inner.blocking_read_for(bound),
        }
    }

    /// Try shared ownership without waiting.
    pub fn try_read_owned(&self) -> Result<PublicationReadPermit, tokio::sync::TryLockError> {
        self.inner
            .try_read_owned()
            .map(|permit| PublicationReadPermit { _permit: permit })
    }

    /// Acquire a short destructive commit on the blocking lane.
    #[track_caller]
    pub fn blocking_write_for(&self, bound: Duration) -> PublicationCommitPermit {
        PublicationCommitPermit {
            _permit: self.inner.blocking_write_for(bound),
        }
    }

    /// Exclude publishers for a bounded checkpoint or orderly shutdown.
    #[track_caller]
    pub fn drain_for(
        &self,
        bound: Duration,
    ) -> impl std::future::Future<Output = PublicationDrainPermit> + use<'_> {
        let waiting = self.inner.write_for(bound);
        async move {
            PublicationDrainPermit {
                _permit: waiting.await,
            }
        }
    }
}
