//! Required, finite liveness budgets for every daemon process session.
use std::io;
use std::time::{Duration, Instant};

/// A short progress stall differs from a hard runtime cap. CPU/output progress
/// permits long compiles, while the finite total also bounds unavailable CPU
/// accounting and downstream output backpressure.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcessLiveness {
    pub(super) stall_window: Duration,
    pub(super) sample_period: Duration,
    pub(super) total: Duration,
    pub(super) post_exit_grace: Duration,
    pub(super) cleanup: Duration,
}

impl ProcessLiveness {
    /// Construct explicit budgets. Zero drain grace abandons immediately;
    /// zero stall, total or cleanup budgets are invalid, never unbounded.
    pub(crate) fn new(
        stall_window: Duration,
        total: Duration,
        post_exit_grace: Duration,
        cleanup: Duration,
    ) -> io::Result<Self> {
        if stall_window.is_zero()
            || total.is_zero()
            || cleanup.is_zero()
            || [stall_window, total, post_exit_grace, cleanup]
                .into_iter()
                .any(|bound| Instant::now().checked_add(bound).is_none())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process liveness requires finite positive stall, runtime and cleanup budgets",
            ));
        }
        Ok(Self {
            stall_window,
            sample_period: (stall_window / 4).clamp(
                Duration::from_millis(1),
                super::super::child_watchdog::stall_tick(),
            ),
            total,
            post_exit_grace,
            cleanup,
        })
    }

    /// Preserve the configured five-minute stall/two-second drain defaults.
    /// Progressing builds may run up to six hours; this cap bounds children
    /// even when CPU observations cannot be obtained. Zero legacy overrides
    /// select the bounded defaults instead of disabling liveness enforcement.
    pub(crate) fn configured() -> Self {
        let fallback = Self {
            stall_window: Duration::from_secs(300),
            sample_period: Duration::from_secs(5),
            total: Duration::from_secs(6 * 60 * 60),
            post_exit_grace: Duration::from_secs(2),
            cleanup: Duration::from_secs(5),
        };
        let positive = |value: Duration, default| if value.is_zero() { default } else { value };
        Self::new(
            positive(
                super::super::child_watchdog::stall_window(),
                fallback.stall_window,
            ),
            fallback.total,
            positive(
                super::super::child_watchdog::post_exit_grace(),
                fallback.post_exit_grace,
            ),
            fallback.cleanup,
        )
        .unwrap_or(fallback)
    }

    pub(crate) fn with_total(self, total: Duration) -> io::Result<Self> {
        Self::new(self.stall_window, total, self.post_exit_grace, self.cleanup)
    }

    pub(crate) fn with_post_exit_grace(self, grace: Duration) -> io::Result<Self> {
        Self::new(self.stall_window, self.total, grace, self.cleanup)
    }
}
