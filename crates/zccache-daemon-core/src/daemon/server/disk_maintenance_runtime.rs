//! Maintenance scheduling, shutdown and session-retention workers.

use super::*;

pub(in crate::daemon::server) async fn maintain_state_disk(
    state: Arc<SharedState>,
    policy: MaintenancePolicy,
    kind: MaintenanceKind,
    runtime_handle: Option<&kernal_api::async_engine::RuntimeHandle>,
) -> io::Result<DiskMaintenanceReport> {
    let maintenance_guard = state.disk_maintenance.acquire_for(OPERATION_TIMEOUT).await;
    if state.shutdown_requested.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "disk maintenance skipped because daemon shutdown is in progress",
        ));
    }
    let root_released = || {
        io::Error::new(
            io::ErrorKind::Interrupted,
            "disk maintenance skipped because cache-root ownership was released",
        )
    };
    // The async owner covers the completion marker; the worker's separate
    // lease covers cleanup even if this future is cancelled.
    let _completion_root = state
        .cache_root_lock
        .lease_for(STATE_TIMEOUT)
        .ok_or_else(root_released)?;
    let worker_root = state
        .cache_root_lock
        .lease_for(STATE_TIMEOUT)
        .ok_or_else(root_released)?;
    let cache_dir = state.cache_dir.clone();
    let index_writer_tx = state.index_writer_tx.clone();
    let maintenance_index_writer_tx = index_writer_tx.clone();
    let pending_write_bytes = state.in_flight_bytes.load(Ordering::Relaxed) as u64;
    let maintenance_state = Arc::clone(&state);
    let (result, _maintenance_guard) = launch_maintenance_blocking(runtime_handle, move || {
        // Capture the root after owners that may touch disk on destruction.
        let _ = &maintenance_state;
        let _ = &maintenance_guard;
        let _root = worker_root;
        let worker_state = maintenance_state;
        let result = (|| {
            let dep_graph = worker_state.dep_graph.load_full();
            let mut report = maintain_disk_artifacts_with_barrier(
                MaintenancePass {
                    artifact_dir: worker_state.artifact_dir.as_path(),
                    artifacts: &worker_state.artifacts,
                    artifact_store: &worker_state.artifact_store,
                    index_writer_tx: Some(&maintenance_index_writer_tx),
                    dep_graph: &dep_graph,
                    pending_write_bytes,
                    policy,
                    kind,
                    environment: &RealMaintenanceEnvironment,
                    retired_top_level: versioned_top_level(worker_state.cache_dir.as_path()),
                },
                Some(&worker_state.artifact_publication),
                Some(&worker_state.shutdown_requested),
            )?;
            report.bytes_reclaimed =
                report
                    .bytes_reclaimed
                    .saturating_add(recover_retired_exec_for(
                        &worker_state.exec_store,
                        crate::core::wait::PROBE,
                    ));
            Ok::<_, io::Error>(report)
        })();
        // The worker retains ownership if its caller is cancelled. On a
        // normal return, keep the permit through WAL acknowledgement too.
        (result, maintenance_guard)
    })
    .await
    .map_err(io::Error::other)?;
    let report = result?;

    if report.artifacts_removed > 0
        && !flush_index_writer(&index_writer_tx, Duration::from_secs(30)).await
    {
        return Err(io::Error::other(
            "artifact-index removals did not flush before the maintenance deadline",
        ));
    }

    if kind == MaintenanceKind::Full {
        record_full_maintenance(cache_dir.as_path(), SystemTime::now())?;
    }
    tracing::info!(
        maintenance_kind = ?report.kind,
        pressure = ?report.pressure,
        budget_bytes = report.budget_bytes,
        usage_before_bytes = report.usage_before_bytes,
        usage_after_bytes = report.usage_after_bytes,
        bytes_reclaimed = report.bytes_reclaimed,
        retired_bytes_reclaimed = report.retired_bytes_reclaimed,
        artifacts_removed = report.artifacts_removed,
        expired_artifacts_removed = report.expired_artifacts_removed,
        pending_write_bytes = report.pending_write_bytes,
        cache_root = %cache_dir.display(),
        "disk maintenance pass complete"
    );
    Ok(report)
}

pub(super) async fn wait_for_artifact_load(state: &SharedState) -> bool {
    loop {
        if state.shutdown_requested.load(Ordering::Acquire) {
            return false;
        }
        if state.artifacts_loaded.load(Ordering::Acquire)
            && state.dep_graph_load_complete.load(Ordering::Acquire)
        {
            return true;
        }
        kernal_api::async_engine::sleep(Duration::from_millis(100)).await;
    }
}

pub(super) fn pressure_scan_needed(
    state: &SharedState,
    policy: MaintenancePolicy,
) -> io::Result<bool> {
    let space = RealMaintenanceEnvironment.filesystem_space(state.artifact_dir.as_path())?;
    if space.free_bytes < low_space_bytes(space.capacity_bytes) {
        return Ok(true);
    }
    let indexed = state.artifact_store.total_size_bytes();
    let pending = state.in_flight_bytes.load(Ordering::Relaxed) as u64;
    // A deliberately early logical gate keeps ordinary five-minute checks
    // memory-only. Near half-budget we pay for exact allocated-block/FileId
    // accounting; daily full passes always do the physical scan.
    Ok(indexed.saturating_add(pending)
        >= policy.budget_bytes(space.capacity_bytes).saturating_mul(50) / 100)
}

/// Wait for the next maintenance pass without consuming the daemon's shared
/// shutdown notification.
///
/// The accept loop also waits on `state.shutdown`. Waiting on that same
/// edge-triggered `Notify` here can both steal a legacy `notify_one()` signal
/// and miss `notify_waiters()` between checking `shutdown_requested` and
/// registering the waiter. Polling the durable atomic flag keeps shutdown
/// bounded without either race.
pub(in crate::daemon::server) async fn wait_for_next_pass_or_shutdown(
    shutdown_requested: &AtomicBool,
    interval: Duration,
    poll_interval: Duration,
) -> bool {
    let deadline = kernal_api::async_engine::Deadline::after(interval);
    loop {
        if shutdown_requested.load(Ordering::Acquire) {
            return true;
        }
        let remaining = deadline.remaining();
        if remaining.is_zero() {
            return false;
        }
        kernal_api::async_engine::sleep(remaining.min(poll_interval)).await;
    }
}

/// Drop daemon-side state for sessions that are over and are never coming
/// back (#1165 Finding 1).
///
/// `SessionManager` has shipped both reapers since it was written, with unit
/// tests, and **no production caller** — so a long-lived daemon held every
/// session's `context_keys` and `stats_tracker` for its whole life. Two ways
/// a session ends without cleanup:
///
/// * the client sent `SessionEnd` and simply went idle afterwards
/// * the client crashed or was killed, so `SessionEnd` never arrived at all
///
/// The second is the one that leaks unboundedly in practice: a cancelled
/// build leaves its whole session behind, and nothing else ever removes it.
///
/// Both reapers are conservative by construction — idle-timeout for the
/// first, process liveness for the second — so this cannot evict a session a
/// live client is still using.
pub(super) fn reap_finished_sessions(state: &SharedState) {
    reap_finished_sessions_with(state, zccache_ipc::is_process_alive);
}

/// [`reap_finished_sessions`] against an explicit liveness predicate.
///
/// The seam exists because "a PID that is reliably dead" is not portable.
/// `is_process_alive` is `kill(pid, 0) == 0` on unix and `OpenProcess` on
/// Windows, and PID 0 disagrees between them: `kill(0, 0)` signals the
/// *caller's own process group* and succeeds, while `OpenProcess(0)` fails.
/// A test using it passes on Windows and fails on Linux. Spawning a real
/// process and killing it would swap that for a PID-recycling race.
///
/// Injecting the predicate makes the reaping logic deterministic on every
/// platform and leaves `is_process_alive` — which has its own tests — as the
/// only thing depending on OS behaviour.
pub(super) fn reap_finished_sessions_with(state: &SharedState, is_alive: impl Fn(u32) -> bool) {
    reap_finished_sessions_with_grace(
        state,
        is_alive,
        crate::depgraph::session::DEAD_CLIENT_REAP_GRACE,
    );
}

/// [`reap_finished_sessions_with`] with the dead-owner grace supplied by the
/// caller, so tests can exercise reclamation without sleeping for it (#1324).
pub(super) fn reap_finished_sessions_with_grace(
    state: &SharedState,
    is_alive: impl Fn(u32) -> bool,
    grace: std::time::Duration,
) {
    let mut expired = state.sessions.cleanup_expired();
    let dead = state.sessions.cleanup_dead_pids_idle_for(is_alive, grace);
    let expired_count = expired.len();
    let dead_count = dead.len();
    expired.extend(dead);
    let tombstones = reap_ended_session_tombstones(state, ENDED_SESSION_TTL);
    let journals_released = release_reaped_session_journals(state, &expired);
    if !expired.is_empty() || tombstones > 0 {
        tracing::info!(
            expired = expired_count,
            dead_client = dead_count,
            journals_released,
            tombstones,
            remaining = state.sessions.active_count(),
            tombstones_remaining = state.ended_sessions.len(),
            "reaped finished sessions"
        );
        // #1165 Finding 1: a `tracing::info!` is lost as soon as the daemon's
        // stderr goes anywhere unbounded. The durable event is what makes
        // "this daemon reclaims a session per compile" attributable after the
        // fact, which is the growth mode the finding is about.
        zccache_core::lifecycle::write_event_in_cache_root(
            state.cache_dir.as_path(),
            zccache_core::lifecycle::EVENT_SESSIONS_REAPED,
            serde_json::json!({
                "expired": expired_count,
                "dead_client": dead_count,
                "journals_released": journals_released,
                "tombstones": tombstones,
                "remaining": state.sessions.active_count(),
                "tombstones_remaining": state.ended_sessions.len(),
            }),
        );
    }
}

/// Reclaim depfile directories left by daemon instances that are gone.
///
/// #1165 Finding 6: the startup sweep in `run.rs` bounds growth *across*
/// daemon lifetimes but not *within* one, and it is on the standalone path
/// only — an embedded host that never restarts the process never swept at all.
/// Running it here fixes both, because `spawn_disk_maintenance` is the one
/// maintenance loop both modes share.
///
/// Full passes only: the directories are age-floored at a week, so sweeping
/// every pressure tick would be scanning for debris that cannot have appeared
/// since the last scan.
///
/// The scan stats every candidate directory and may `remove_dir_all`, so it
/// runs on the blocking pool rather than an executor worker.
pub(super) async fn sweep_stale_depfile_dirs(
    runtime_handle: Option<&kernal_api::async_engine::RuntimeHandle>,
) {
    let cleaned = launch_maintenance_blocking(runtime_handle, || {
        crate::core::config::cleanup_stale_depfile_dirs(crate::ipc::is_process_alive)
    })
    .await;
    let cleaned = match cleaned {
        Ok(cleaned) => cleaned,
        Err(error) => {
            tracing::warn!(%error, "stale depfile sweep worker failed");
            return;
        }
    };
    if cleaned > 0 {
        tracing::info!(cleaned, "swept stale depfile directories");
        zccache_core::lifecycle::write_event(
            zccache_core::lifecycle::EVENT_STALE_DEPFILE_DIRS_SWEPT,
            serde_json::json!({ "cleaned": cleaned }),
        );
    }
}

/// Reclaim retired sibling `v<VERSION>` cache stores (issue #1659).
///
/// Nothing previously reclaimed a store an upgrade left behind except an
/// explicit `zccache clear`; a daemon that never had that run against it
/// accumulates one full store per upgrade forever. This runs every pass this
/// loop takes (`spawn_disk_maintenance`'s body executes immediately on the
/// first iteration, so that pass doubles as the "once at daemon start"
/// sweep), independent of the current store's own pressure/full gating below
/// -- a retired store can hold tens of GB while the live store reports
/// comfortably under budget.
///
/// Only fires when `cache_dir` is itself a real versioned store, i.e. its
/// final path segment is exactly `v<crate::core::VERSION>`. A cache root
/// bound straight to a bare directory (every non-versioned test harness in
/// this crate) has no sibling-version layout to sweep and is left alone --
/// there is no safe "top level" to infer from a path that was never
/// `<top>/v<VERSION>` to begin with.
pub(super) async fn sweep_retired_version_stores(
    cache_dir: &Path,
    runtime_handle: Option<&kernal_api::async_engine::RuntimeHandle>,
) {
    let Some((top_level, current)) = versioned_top_level(cache_dir) else {
        return;
    };

    // #1673: periodic stamp -- keep the running store's `.last-active`
    // fresh so a sibling daemon of another version never treats it as idle.
    if let Err(error) = crate::core::config::touch_store_activity_marker(cache_dir) {
        tracing::debug!(%error, cache_root = %cache_dir.display(), "failed to stamp store activity marker");
    }
    let report = launch_maintenance_blocking(runtime_handle, move || {
        crate::core::config::sweep_retired_version_stores_in_with_mode(
            &top_level,
            &current,
            RETIRED_STORE_MAX_AGE,
            SystemTime::now(),
            crate::core::config::RetiredSweepMode::Routine,
        )
    })
    .await;
    let report = match report {
        Ok(report) => report,
        Err(error) => {
            tracing::warn!(%error, "retired-store sweep worker failed");
            return;
        }
    };
    if report.stores_removed == 0 && report.files_removed == 0 && report.failed == 0 {
        return;
    }
    tracing::info!(
        stores_scanned = report.stores_scanned,
        stores_removed = report.stores_removed,
        stores_live = report.stores_live,
        files_removed = report.files_removed,
        bytes_reclaimed = report.bytes_reclaimed,
        failed = report.failed,
        "reclaimed retired zccache version stores"
    );
    zccache_core::lifecycle::write_event_in_cache_root(
        cache_dir,
        zccache_core::lifecycle::EVENT_RETIRED_STORES_SWEPT,
        serde_json::json!({
            "stores_scanned": report.stores_scanned,
            "stores_removed": report.stores_removed,
            "stores_live": report.stores_live,
            "files_removed": report.files_removed,
            "bytes_reclaimed": report.bytes_reclaimed,
            "failed": report.failed,
        }),
    );
}

/// Drop `ended_sessions` entries older than `ttl`. Returns how many went.
///
/// Separate from the session reapers above because it ages on the daemon's own
/// clock rather than on client liveness — a tombstone outlives the client by
/// definition, so "is the pid alive" says nothing about whether it is still
/// needed.
pub(super) fn reap_ended_session_tombstones(state: &SharedState, ttl: Duration) -> usize {
    reap_ended_session_tombstones_at(state, ttl, std::time::Instant::now())
}

/// [`reap_ended_session_tombstones`] against an explicit observation time.
///
/// The seam exists so tests can place stamps relative to a base by **adding**
/// to an `Instant` instead of subtracting from `Instant::now()`. Subtracting
/// an hour panics when the monotonic epoch is younger than an hour — a fresh
/// CI runner — which is the same trap `SessionManager::cleanup_expired`
/// already guards with `checked_sub` ("timeout exceeds uptime; nothing can be
/// expired"). Addition cannot underflow, so the test states the boundary
/// directly with no clock arithmetic that can fail.
pub(super) fn reap_ended_session_tombstones_at(
    state: &SharedState,
    ttl: Duration,
    now: std::time::Instant,
) -> usize {
    let before = state.ended_sessions.len();
    state
        .ended_sessions
        // Saturating: a stamp ahead of `now` yields zero age and is kept,
        // rather than panicking. That cannot happen from the insert site, but
        // this runs every maintenance tick for the daemon's life and a panic
        // here would take the maintenance task down with it.
        .retain(|_, ended_at| now.saturating_duration_since(*ended_at) < ttl);
    before.saturating_sub(state.ended_sessions.len())
}

pub(in crate::daemon::server) fn spawn_disk_maintenance(
    state: Arc<SharedState>,
    policy: MaintenancePolicy,
    runtime_handle: Option<&kernal_api::async_engine::RuntimeHandle>,
) -> kernal_api::async_engine::Task<()> {
    let spawn_handle = runtime_handle.cloned();
    let runtime_handle = runtime_handle.cloned();
    let task = async move {
        if !wait_for_artifact_load(&state).await {
            return;
        }
        loop {
            if state.shutdown_requested.load(Ordering::Acquire) {
                break;
            }
            // #1659: unconditional, every pass -- a retired store must not
            // wait behind the current store's own pressure/full gating.
            sweep_retired_version_stores(state.cache_dir.as_path(), runtime_handle.as_ref()).await;
            // #1165 Finding 1: reap before the pressure gate, not after the
            // disk pass. The `Ok(false)` branch below `continue`s when the
            // cache is under the preflight threshold, so a reap placed after
            // it would be skipped on exactly the quiet daemons that live
            // longest and accumulate the most dead sessions.
            reap_finished_sessions(&state);
            let kind = if full_maintenance_due(state.cache_dir.as_path(), SystemTime::now()) {
                MaintenanceKind::Full
            } else {
                MaintenanceKind::Pressure
            };
            if kind == MaintenanceKind::Full {
                sweep_stale_depfile_dirs(runtime_handle.as_ref()).await;
            }
            if kind == MaintenanceKind::Pressure {
                match pressure_scan_needed(&state, policy) {
                    Ok(false) => {
                        tracing::debug!(
                            cache_root = %state.cache_dir.display(),
                            "disk maintenance skipped exact scan below logical preflight threshold"
                        );
                        if wait_for_next_pass_or_shutdown(
                            &state.shutdown_requested,
                            PRESSURE_INTERVAL,
                            SHUTDOWN_POLL_INTERVAL,
                        )
                        .await
                        {
                            break;
                        }
                        continue;
                    }
                    Ok(true) => {}
                    Err(error) => tracing::warn!(%error, "disk maintenance preflight failed"),
                }
            }
            let outcome =
                maintain_state_disk(Arc::clone(&state), policy, kind, runtime_handle.as_ref())
                    .await;
            super::disk_maintenance_progress::record_pass(
                &state.disk_maintenance_progress,
                kind == MaintenanceKind::Full,
                &outcome,
            );
            if let Err(error) = outcome {
                tracing::warn!(
                    %error,
                    maintenance_kind = ?kind,
                    cache_root = %state.cache_dir.display(),
                    "disk maintenance pass failed"
                );
            }
            if state.shutdown_requested.load(Ordering::Acquire) {
                break;
            }
            if wait_for_next_pass_or_shutdown(
                &state.shutdown_requested,
                PRESSURE_INTERVAL,
                SHUTDOWN_POLL_INTERVAL,
            )
            .await
            {
                break;
            }
        }
    };
    match spawn_handle {
        Some(handle) => handle.launch(task),
        None => kernal_api::async_engine::launch(task),
    }
}

/// Runs only on the blocking cleanup lane, outside publication exclusion.
pub(in crate::daemon::server) fn recover_retired_exec_for(store: &KvStore, bound: Duration) -> u64 {
    match store.sweep_retired_namespaces_for(bound) {
        Ok(report) => {
            if report.failures > 0 || report.deferred {
                tracing::warn!(
                    failures = report.failures,
                    deferred = report.deferred,
                    "retired exec namespaces retained for the next maintenance pass"
                );
            }
            report.reclaimed_bytes
        }
        Err(error) => {
            tracing::warn!(%error, "retired exec namespace recovery deferred");
            0
        }
    }
}
