//! Exec single-flight ownership and bounded waiter registration.
use super::*;
use dashmap::mapref::entry::Entry;

pub(super) enum InFlight {
    /// We acquired the slot ourselves (no peer was running).
    Owner,
    /// We waited on a peer's `Notify`; caller should re-attempt the cache
    /// lookup before spawning the tool.
    WokenByPeer,
}

pub(super) struct InFlightGuardExec {
    pub(super) state: Arc<SharedState>,
    pub(super) key: String,
    pub(super) outcome: InFlight,
}

impl Drop for InFlightGuardExec {
    fn drop(&mut self) {
        if matches!(self.outcome, InFlight::Owner) {
            // Remove the slot, then wake any waiters.
            if let Some((_, notify)) = self.state.in_flight_exec.remove(&self.key) {
                notify.notify_waiters();
            }
        }
    }
}

pub(super) async fn acquire_in_flight(
    state: &Arc<SharedState>,
    key_hex: &str,
) -> InFlightGuardExec {
    let key = key_hex.to_string();
    let notify_arc = {
        match state.in_flight_exec.entry(key.clone()) {
            Entry::Occupied(o) => Arc::clone(o.get()),
            Entry::Vacant(v) => {
                v.insert(Arc::new(Notify::new()));
                return InFlightGuardExec {
                    state: Arc::clone(state),
                    key,
                    outcome: InFlight::Owner,
                };
            }
        }
    };

    let budget = in_flight_wait_budget();
    if let CoalesceOutcome::TimedOut =
        coalesce_wait(&state.in_flight_exec, &key, notify_arc, budget).await
    {
        // Loud + durable: a wedged owner made the herd fall back to running
        // their own copies. Logged here (not in `coalesce_wait`) so the pure
        // wait helper stays unit-testable without touching the lifecycle log.
        tracing::warn!(
            event = "in_flight_exec_wait_timeout",
            key = %key,
            budget_ms = budget.as_millis() as u64,
            "waited past the coalesce budget for the in-flight exec owner of this key; \
             the owner may be wedged — running our own copy instead of hanging (issue #971)"
        );
        crate::core::lifecycle::write_event(
            crate::core::lifecycle::EVENT_IN_FLIGHT_EXEC_WAIT_TIMEOUT,
            serde_json::json!({
                "key": key,
                "budget_ms": budget.as_millis() as u64,
                "reason": "in-flight exec owner did not finish within the coalesce budget; running own copy",
            }),
        );
    }
    InFlightGuardExec {
        state: Arc::clone(state),
        key,
        outcome: InFlight::WokenByPeer,
    }
}

/// Outcome of [`coalesce_wait`].
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CoalesceOutcome {
    /// The owner's `Notify` fired while we were parked.
    Woken,
    /// The slot was already gone or replaced by a new owner when we re-checked
    /// after registering — no wait was needed.
    SlotResolved,
    /// We waited the full budget without a wakeup (owner likely wedged).
    TimedOut,
}

/// Wait for the in-flight owner of `key` to finish, bounded by `budget`.
///
/// Closes the two canonical `Notify` single-flight hazards behind #971:
/// - **Lost wakeup (mode 1):** `Notified` only arms a waiter once polled, so we
///   `enable()` the future BEFORE re-checking the map. An owner that runs
///   `remove()` + `notify_waiters()` between the caller cloning the `Arc` and
///   this point would otherwise have its wakeup dropped (`notify_waiters`
///   stores no permit), stranding the waiter forever.
/// - **Slot replaced (mode 2):** after `enable()` we re-check by identity
///   (`Arc::ptr_eq`); if the slot is gone or a *new* owner installed a fresh
///   `Notify`, the `notify_arc` we hold will never fire again, so we return
///   `SlotResolved` instead of parking on a dead notify.
///
/// Kept free of `SharedState` and logging so it is deterministically testable
/// against a bare `DashMap`.
pub(super) async fn coalesce_wait(
    in_flight: &dashmap::DashMap<String, Arc<Notify>>,
    key: &str,
    notify_arc: Arc<Notify>,
    budget: std::time::Duration,
) -> CoalesceOutcome {
    let notified = notify_arc.notified();
    let mut notified = std::pin::pin!(notified);
    notified.as_mut().enable();

    let still_ours = in_flight
        .get(key)
        .is_some_and(|cur| Arc::ptr_eq(cur.value(), &notify_arc));
    if !still_ours {
        return CoalesceOutcome::SlotResolved;
    }

    let deadline = kernal_api::async_engine::sleep(budget);
    let mut deadline = std::pin::pin!(deadline);
    match kernal_api::fair_race!((notified.as_mut()), (deadline.as_mut())).await {
        kernal_api::async_engine::FairRace2::First(()) => CoalesceOutcome::Woken,
        kernal_api::async_engine::FairRace2::Second(()) => CoalesceOutcome::TimedOut,
    }
}

/// Coalesce wait budget for [`acquire_in_flight`]. A peer already running the
/// same keyed tool is usually the fast path, but a wedged owner must not hang
/// its waiters forever — on expiry the waiter runs its own copy. Generous by
/// default (exec tools can legitimately run a while); override with
/// `ZCCACHE_EXEC_COALESCE_WAIT_MS`.
fn in_flight_wait_budget() -> std::time::Duration {
    std::env::var(EXEC_COALESCE_WAIT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_millis(
            EXEC_COALESCE_WAIT_DEFAULT_MS,
        ))
}
