//! `DaemonServer::run` — the daemon's main loop, plus the file-watcher
//! pipeline initializer it kicks off.
//!
//! Owns startup-side cleanup of legacy state, the four background tasks
//! (artifact load, memory eviction, disk GC, depgraph save), and the
//! shutdown drain that persists artifact-store, depgraph, and metadata
//! caches to disk.

use super::*;

const ACCEPT_STALL_WATCHDOG_INTERVAL: Duration = Duration::from_secs(600);
const MEMORY_GC_IDLE_GRACE: Duration = Duration::from_millis(250);
const MEMORY_GC_GENTLE_RETRY: Duration = Duration::from_millis(50);
const MEMORY_GC_FORCE_AFTER: Duration = Duration::from_secs(5);
const MEMORY_GC_GENTLE_BATCH: usize = 256;
/// Startup budget for taking the watcher lock before declaring degradation.
const WATCHER_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
/// First re-arm attempt after a failed watcher init (issue #1156). Cheap
/// enough to be always-on, so no config knob is exposed.
const WATCHER_REARM_INITIAL_DELAY: Duration = Duration::from_secs(60);
/// Backoff ceiling for watcher re-arm retries.
const WATCHER_REARM_MAX_DELAY: Duration = Duration::from_secs(600);

enum AcceptWait<T> {
    Accepted(T),
    Shutdown,
}

/// Wait for accept or shutdown while reporting stalls without cancelling
/// either operation. Windows accept futures own their pending pipe instance,
/// and `Notify::notified` is not buffered, so both futures must survive every
/// observational watchdog tick.
async fn wait_for_accept_or_shutdown<A, S, T>(
    accept: A,
    shutdown: S,
    watchdog_interval: Duration,
) -> AcceptWait<T>
where
    A: std::future::Future<Output = T>,
    S: std::future::Future<Output = ()>,
{
    let mut accept = std::pin::pin!(accept);
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        let watchdog = kernal_api::async_engine::sleep(watchdog_interval);
        let mut watchdog = std::pin::pin!(watchdog);
        match kernal_api::fair_race!((accept.as_mut()), (watchdog.as_mut()), (shutdown.as_mut()),)
            .await
        {
            kernal_api::async_engine::FairRace3::First(result) => {
                return AcceptWait::Accepted(result);
            }
            kernal_api::async_engine::FairRace3::Second(()) => {
                tracing::warn!(
                    stall_secs = watchdog_interval.as_secs(),
                    "daemon accept loop has not accepted a connection within watchdog interval"
                );
            }
            kernal_api::async_engine::FairRace3::Third(()) => return AcceptWait::Shutdown,
        }
    }
}

impl DaemonServer {
    /// Run the server, accepting connections until shutdown is signaled.
    ///
    /// `idle_timeout_secs`: if non-zero, the daemon shuts down after this many
    /// seconds with no client activity. Pass 0 to disable.
    pub async fn run(&mut self, idle_timeout_secs: u64) -> Result<(), crate::ipc::IpcError> {
        let maintenance_policy =
            MaintenancePolicy::from_env().map_err(crate::ipc::IpcError::Endpoint)?;
        clear_shutdown_shortfall(&self.state);
        tracing::info!(
            persist_workers = self.state.persist_semaphore.available_permits(),
            "daemon server running"
        );

        // Background index-writer task: in-memory WAL with timer-driven
        // flushing. See `run_index_writer` for the design rationale.
        let mut index_writer_handle: Option<kernal_api::async_engine::Task<()>> = None;
        if let Some(rx) = self.index_writer_rx.take() {
            let store = Arc::clone(&self.state.artifact_store);
            let shutdown = Arc::clone(&self.state.index_writer_shutdown);
            let root = self
                .state
                .cache_root_lock
                .lease_for(STATE_TIMEOUT)
                .expect("index writer startup requires an owned cache root");
            let runtime = kernal_api::async_engine::RuntimeHandle::current()
                .expect("daemon run requires an async runtime");
            index_writer_handle = Some(runtime.launch(run_index_writer(rx, store, shutdown, root)));
        }

        let cache_dir = self.state.cache_dir.clone();

        // Migrate legacy blob digests once per cache root. Keep this off the
        // Tokio runtime thread because the helper hashes files synchronously.
        // The marker is published only after a successful migration so a
        // failed attempt remains retryable on the next daemon start.
        //
        // The staged-artifact temp sweep that used to live here is now a
        // member of `MaintenanceSchedule` so the embedded service runs it too
        // (#1160c).
        {
            let cache_dir = cache_dir.clone();
            let artifact_dir = self.state.artifact_dir.clone();
            let state = Arc::clone(&self.state);
            kernal_api::async_engine::launch(async move {
                if let Err(error) = state.staging.cleanup_abandoned() {
                    tracing::debug!(%error, "abandoned private staging cleanup skipped");
                }
                let marker = cache_dir.join(".legacy-blob-digests-migrated-v1");
                if marker.exists() {
                    tracing::debug!(path = %marker.display(), "legacy blob digest migration already complete");
                    return;
                }

                let migration_root = artifact_dir;
                let result = kernal_api::async_engine::launch_blocking(move || {
                    let migrated = migrate_legacy_blob_digests(&migration_root)?;
                    use std::io::Write;
                    let mut marker_file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&marker)?;
                    marker_file.write_all(b"v1\n")?;
                    marker_file.sync_all()?;
                    Ok::<usize, std::io::Error>(migrated)
                })
                .await;

                match result {
                    Ok(Ok(migrated)) => {
                        tracing::info!(migrated, "legacy blob digest migration complete")
                    }
                    Ok(Err(error)) => tracing::warn!(
                        path = %cache_dir.display(),
                        "legacy blob digest migration failed: {error}"
                    ),
                    Err(error) => {
                        tracing::warn!("legacy blob digest migration task failed: {error}")
                    }
                }
            }).detach();
        }

        // Clean up legacy log backup directory (Bug 7).
        {
            let legacy_logs = cache_dir.join("logs.bak");
            if legacy_logs.is_dir() {
                match std::fs::remove_dir_all(&legacy_logs) {
                    Ok(()) => tracing::info!("removed legacy logs.bak directory"),
                    Err(e) => tracing::warn!(
                        path = %legacy_logs.display(),
                        "failed to remove legacy logs.bak: {e}"
                    ),
                }
            }
            // Also remove stale daemon.lock.bak if present.
            let legacy_lock = cache_dir.join("daemon.lock.bak");
            let _ = std::fs::remove_file(&legacy_lock);
        }

        // Legacy temp-root state removal is a standalone-only member of
        // `MaintenanceSchedule` (see its rationale) and starts below.

        // Clean up stale depfile directories from dead daemon instances.
        {
            let cleaned =
                crate::core::config::cleanup_stale_depfile_dirs(crate::ipc::is_process_alive);
            if cleaned > 0 {
                tracing::info!(cleaned, "cleaned stale depfile directories");
            }
        }

        self.start_watcher_pipeline().await;

        // Start background artifact loading (non-blocking so daemon responds
        // immediately — Bug 6 fix).
        {
            spawn_artifact_loader(Arc::clone(&self.state), None)
                .await
                .detach();
        }

        // Every periodic loop this daemon owns starts here, from the single
        // schedule the embedded service also starts (#1160). Nothing periodic
        // may be hand-spawned in this function any more: a task added here
        // alone would silently not run inside an embedded host, which is the
        // exact drift the schedule exists to make impossible.
        //
        // The standalone-only members (legacy temp-root cleanup, idle
        // watchdog, private-daemon owner reaping) carry their rationale in
        // `MAINTENANCE_TASKS`.
        let started = MaintenanceSchedule::new(
            Arc::clone(&self.state),
            maintenance_policy,
            ServiceMode::Standalone,
        )
        .with_idle_timeout_secs(idle_timeout_secs)
        .start();
        tracing::debug!(tasks = ?started.started, "maintenance schedule started");
        let mut maintenance_handle = started.disk_maintenance;
        let mut depgraph_save_handle = started.depgraph_save;

        loop {
            // Keep one accept future alive across watchdog ticks. On Windows,
            // `Listener::accept` owns a pre-created named-pipe instance while
            // it waits. Dropping that future at every watchdog interval closes
            // the pipe underneath a client that won the connect race, yielding
            // ERROR_NO_DATA / ERROR_BROKEN_PIPE even though the daemon itself
            // remains healthy. The watchdog is observational only: it must not
            // cancel and recreate the accept operation.
            // #1984: `notify_waiters` is not buffered, so a Shutdown handler
            // signalling between two iterations wakes nobody. Create the
            // waiter first, then read the flag every signaller stores before
            // notifying: a signal before this point is seen by the load, and
            // one after it wakes the waiter.
            let shutdown_wait = self.shutdown.notified();
            let winner = if self.state.shutdown_requested.load(Ordering::Acquire) {
                AcceptWait::Shutdown
            } else {
                wait_for_accept_or_shutdown(
                    self.listener.accept(),
                    shutdown_wait,
                    ACCEPT_STALL_WATCHDOG_INTERVAL,
                )
                .await
            };
            match winner {
                AcceptWait::Accepted(result) => {
                    let conn = match result {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::error!("accept failed, continuing: {e}");
                            continue;
                        }
                    };
                    let state = Arc::clone(&self.state);
                    kernal_api::async_engine::launch(async move {
                        if let Err(e) = handle_connection(conn, state).await {
                            tracing::warn!("connection error: {e}");
                        }
                    })
                    .detach();
                }
                AcceptWait::Shutdown => {
                    self.state.shutdown_requested.store(true, Ordering::Release);
                    // `shutdown_handle()` exposes the raw Notify for legacy
                    // tests and Ctrl+C handlers, many of which still call
                    // notify_one(). Rebroadcast so whichever task observes the
                    // edge first wakes the rest of the shutdown path.
                    self.state.shutdown.notify_waiters();
                    shutdown::run(
                        &self.state,
                        maintenance_handle.take(),
                        depgraph_save_handle.take(),
                        index_writer_handle.take(),
                    )
                    .await;

                    return Ok(());
                }
            }
        }
    }

    /// Initialize the file watcher pipeline:
    /// `NotifyWatcher (OS thread) → SettleBuffer (tokio task) → CacheSystem consumer (tokio task)`
    async fn start_watcher_pipeline(&self) {
        if arm_watcher_pipeline(&self.state).await {
            return;
        }
        // Watcher init failed. Without a re-arm this is a lifetime sentence:
        // both fast hit tiers bail on `watcher_active == false` and every
        // shared blob is fully re-hashed on every hit, forever, off a single
        // startup warn line. The common triggers (inotify `max_user_watches`
        // exhaustion, a momentarily contended watcher lock) are transient, so
        // retry on a capped backoff until the daemon shuts down (issue #1156).
        let state = Arc::clone(&self.state);
        kernal_api::async_engine::launch(async move {
            let mut delay = WATCHER_REARM_INITIAL_DELAY;
            let mut attempt = 0_u32;
            while !state.shutdown_requested.load(Ordering::Acquire) {
                let winner = {
                    let sleep = kernal_api::async_engine::sleep(delay);
                    let shutdown = state.shutdown.notified();
                    let mut sleep = std::pin::pin!(sleep);
                    let mut shutdown = std::pin::pin!(shutdown);
                    kernal_api::fair_race!((sleep.as_mut()), (shutdown.as_mut())).await
                };
                match winner {
                    kernal_api::async_engine::FairRace2::First(()) => {}
                    kernal_api::async_engine::FairRace2::Second(()) => {
                        state.shutdown_requested.store(true, Ordering::Release);
                        state.shutdown.notify_waiters();
                        return;
                    }
                }
                if state.shutdown_requested.load(Ordering::Acquire) {
                    return;
                }
                attempt += 1;
                if arm_watcher_pipeline(&state).await {
                    tracing::info!(
                        event = "watcher_rearmed",
                        attempt,
                        "file watcher re-armed after startup failure — fast hit tiers restored"
                    );
                    crate::core::lifecycle::write_event(
                        crate::core::lifecycle::EVENT_WATCHER_REARMED,
                        serde_json::json!({ "attempt": attempt }),
                    );
                    return;
                }
                delay = (delay * 2).min(WATCHER_REARM_MAX_DELAY);
            }
        })
        .detach();
    }
}

/// Attempts one full arm of the watcher pipeline. Returns `true` when the
/// watcher is live and the settle/consumer tasks are running; `false` leaves
/// the daemon in the degraded (watcher-less) mode with the degradation
/// recorded loudly.
pub(super) async fn arm_watcher_pipeline(state: &Arc<SharedState>) -> bool {
    let ignore = Arc::new(crate::watcher::IgnoreFilter::default());
    let (watcher, raw_rx) = match NotifyWatcher::new(ignore) {
        Ok(w) => w,
        Err(e) => {
            record_watcher_degradation(state, &format!("watcher init failed: {e}"));
            return false;
        }
    };

    match state
        .watcher
        .try_with_for(WATCHER_LOCK_TIMEOUT, |slot| *slot = Some(watcher))
        .await
    {
        Ok(()) => {}
        Err(_) => {
            record_watcher_degradation(state, "timed out acquiring watcher lock");
            return false;
        }
    }
    set_registry_watcher_available(true);
    state.watcher_active.store(true, Ordering::Release);
    start_watcher_tasks(state, raw_rx);
    tracing::info!("file watcher pipeline started");
    true
}

/// Loud-forensics degradation record: a `warn!` plus a durable lifecycle
/// event plus a monotonic counter surfaced in `zccache status`, so a daemon
/// running without a watcher is visible without trawling startup logs.
pub(super) fn record_watcher_degradation(state: &Arc<SharedState>, reason: &str) {
    set_registry_watcher_available(false);
    state.watcher_active.store(false, Ordering::Release);
    let degradations = state.watcher_degradations.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::warn!(
        event = "watcher_degraded",
        reason,
        degradations,
        "running without file watcher — fast hit tiers disabled and shared \
         blobs re-hash on every read; will retry"
    );
    crate::core::lifecycle::write_event(
        crate::core::lifecycle::EVENT_WATCHER_DEGRADED,
        serde_json::json!({
            "reason": reason,
            "degradations": degradations,
        }),
    );
}

/// Spawns the settle buffer and the `CacheSystem` consumer over a freshly
/// armed watcher's raw event stream.
pub(super) fn start_watcher_tasks(
    state: &Arc<SharedState>,
    raw_rx: kernal_api::async_engine::UnboundedReceiver<crate::watcher::WatchEvent>,
) {
    {
        // Settle buffer: coalesces raw events into batches after a quiet period.
        let (settled_tx, mut settled_rx) = kernal_api::async_engine::unbounded_channel();
        let settle = SettleBuffer::default_window();
        kernal_api::async_engine::launch(async move {
            settle.run(raw_rx, settled_tx).await;
        })
        .detach();

        // Consumer: feeds settled events into CacheSystem for metadata invalidation.
        let supervised = Arc::clone(state);
        let state = Arc::clone(state);
        let consumer = kernal_api::async_engine::launch(async move {
            while !state.shutdown_requested.load(Ordering::Acquire) {
                // Race the settled-event recv against the shutdown signal
                // (issue #974). Without this the loop only re-checks
                // `shutdown_requested` AFTER `recv()` returns, so if the settle
                // task neither sends nor drops its sender the consumer parks
                // forever and ignores shutdown — a shutdown-cleanliness gap.
                let winner = {
                    let event = settled_rx.recv_within(zccache_core::wait::LIFECYCLE_POLL);
                    let shutdown = state.shutdown.notified();
                    let mut event = std::pin::pin!(event);
                    let mut shutdown = std::pin::pin!(shutdown);
                    kernal_api::fair_race!((event.as_mut()), (shutdown.as_mut())).await
                };
                let event = match winner {
                    kernal_api::async_engine::FairRace2::First(Ok(event)) => Some(event),
                    kernal_api::async_engine::FairRace2::First(Err(
                        kernal_api::async_engine::RecvTimeoutError::TimedOut,
                    )) => continue,
                    kernal_api::async_engine::FairRace2::First(Err(
                        kernal_api::async_engine::RecvTimeoutError::Disconnected,
                    )) => None,
                    kernal_api::async_engine::FairRace2::Second(()) => {
                        state.shutdown_requested.store(true, Ordering::Release);
                        // See the accept-loop shutdown branch: consuming a
                        // single Notify edge here must still wake the loop.
                        state.shutdown.notify_waiters();
                        None
                    }
                };
                let Some(event) = event else { break };
                match event {
                    SettledEvent::Batch { changed, removed } => {
                        let count = changed.len() + removed.len();
                        if count > 0 {
                            tracing::debug!(
                                changed = changed.len(),
                                removed = removed.len(),
                                "watcher batch applied"
                            );
                            // On Windows, notify reports paths with \\?\
                            // extended-length prefix but the rest of the
                            // codebase uses plain paths. Strip the prefix
                            // so journal/metadata lookups match.
                            let (changed, removed) = {
                                let strip =
                                    |paths: Vec<NormalizedPath>| -> Vec<NormalizedPath> {
                                        paths
                                            .into_iter()
                                            .map(|p| {
                                                NormalizedPath::new(
                                            crate::platform::fs::path::strip_verbatim_prefix(&p),
                                        )
                                            })
                                            .collect()
                                    };
                                (strip(changed), strip(removed))
                            };
                            #[cfg(debug_assertions)]
                            for p in changed.iter().chain(removed.iter()) {
                                debug_assert!(
                                    !p.to_string_lossy().starts_with(r"\\?\"),
                                    "watcher path must not have \\\\?\\ prefix: {}",
                                    p.display()
                                );
                            }
                            mark_registered_links_suspect(
                                changed.iter().map(|path| path.as_path()),
                            );
                            mark_removed_links_suspect(removed.iter().map(|path| path.as_path()));
                            state.fingerprint.on_batch(&changed, &removed);
                            state
                                .cache_system
                                .apply_changes_with_removals(changed, removed);
                        }
                    }
                    SettledEvent::Overflow => {
                        let sweep = mark_changed_registered_links_suspect();
                        tracing::warn!(
                            event = "watcher_overflow",
                            links_unchanged = sweep.unchanged,
                            links_suspect = sweep.suspect,
                            "watcher overflow — downgrading all metadata; \
                             hardlink registry re-verified by stat signature"
                        );
                        crate::core::lifecycle::write_event(
                            crate::core::lifecycle::EVENT_WATCHER_OVERFLOW,
                            serde_json::json!({
                                "links_unchanged": sweep.unchanged,
                                "links_suspect": sweep.suspect,
                            }),
                        );
                        state.cache_system.apply_overflow();
                    }
                }
            }
            tracing::debug!("watcher consumer task exiting");
        });

        // #1177 part B: the consumer's handle used to be dropped, so a panic
        // here killed metadata invalidation silently and the daemon kept
        // serving fast hits from state nothing was invalidating any more —
        // stale hits, i.e. a correctness failure, until the next restart.
        //
        // It cannot simply be restarted: the task owns `settled_rx`, which a
        // panic drops, so a fresh loop would have no events to consume and
        // would look healthy while delivering nothing.
        //
        // Instead, fail the way an unarmable watcher already fails (#1156):
        // clear `watcher_active`, which both fast-hit tiers gate on, and say
        // so. That converts a silent correctness bug into the documented
        // degradation the rest of the daemon already handles — slower, but
        // never wrong.
        kernal_api::async_engine::launch(async move {
            let outcome = consumer.await;
            if supervised.shutdown_requested.load(Ordering::Acquire) {
                return;
            }
            let reason = match &outcome {
                Err(err) if err.is_panic() => "consumer_panicked",
                Err(_) => "consumer_cancelled",
                Ok(()) => "consumer_exited",
            };
            supervised.watcher_active.store(false, Ordering::Release);
            tracing::warn!(
                event = "watcher_degraded",
                reason,
                "watcher consumer stopped before shutdown — metadata invalidation \
                 is no longer running; both fast hit tiers are disabled so hits \
                 are re-verified instead of trusted"
            );
            crate::core::lifecycle::write_event(
                crate::core::lifecycle::EVENT_WATCHER_DEGRADED,
                serde_json::json!({
                    "reason": reason,
                    "degradations": ["fast_hit_tiers_disabled", "metadata_invalidation_stopped"],
                }),
            );
        })
        .detach();
    }
}

/// One memory-budget eviction sweep. Driven by the `memory-eviction` member of
/// [`super::maintenance_schedule::MaintenanceSchedule`] in both service modes.
pub(super) async fn run_memory_eviction_pass(
    state: &Arc<SharedState>,
    budget: u64,
) -> (u64, usize) {
    let started = Instant::now();
    let mut total_freed = 0_u64;
    let mut total_items = 0_usize;
    let mut candidate_offset = 0_usize;
    let plan = {
        let dep_graph_guard = state.dep_graph.load();
        let plan = super::super::eviction::plan_eviction_to_budget(
            budget,
            &state.cache_system,
            &dep_graph_guard,
            &state.fast_hit_cache,
            &state.artifacts,
            state.in_flight_bytes.load(Ordering::Relaxed),
        );
        drop(dep_graph_guard);
        plan
    };
    let Some(plan) = plan else {
        return (0, 0);
    };
    let mut gentle_finished = false;
    let mut busy_candidates = Vec::new();
    let mut protected_paths = std::collections::HashSet::new();

    loop {
        let active_requests = state.active_cache_requests();
        if active_requests == 0 || started.elapsed() >= MEMORY_GC_FORCE_AFTER {
            // Nonblocking metadata removal intentionally leaves orphaned
            // journal rows. Settle that debt before deciding whether the
            // original headroom target still requires destructive work.
            let journal_removed = state.cache_system.cleanup_eviction_journal();
            total_freed += (journal_removed * super::super::eviction::JOURNAL_ENTRY_BYTES) as u64;
            total_items += journal_removed;

            let dep_graph_guard = state.dep_graph.load();
            let current = super::super::eviction::memory_snapshot(
                &state.cache_system,
                &dep_graph_guard,
                &state.fast_hit_cache,
                &state.artifacts,
                state.in_flight_bytes.load(Ordering::Relaxed),
            );
            drop(dep_graph_guard);
            if current.total_bytes as u64 <= plan.target_bytes() {
                return (total_freed, total_items);
            }

            // Retry lock-busy candidates in blocking mode, preserve only
            // timestamp-refreshed entries, then replenish from a current
            // read-only plan until the target is met or every remaining
            // metadata candidate is protected for this sweep.
            let mut completion_plan = plan.completion_from(candidate_offset, &busy_candidates);
            loop {
                let completion_had_candidates = completion_plan.metadata_candidate_count() > 0;
                let dep_graph_guard = state.dep_graph.load();
                let outcome = super::super::eviction::evict_to_budget_with_plan_detailed(
                    budget,
                    &state.cache_system,
                    &dep_graph_guard,
                    &state.fast_hit_cache,
                    &state.artifacts,
                    state.in_flight_bytes.load(Ordering::Relaxed),
                    &completion_plan,
                );
                drop(dep_graph_guard);
                total_freed += outcome.freed_bytes;
                total_items += outcome.items_removed;
                protected_paths.extend(
                    outcome
                        .refreshed
                        .iter()
                        .map(|candidate| candidate.path().clone()),
                );

                let dep_graph_guard = state.dep_graph.load();
                let current = super::super::eviction::memory_snapshot(
                    &state.cache_system,
                    &dep_graph_guard,
                    &state.fast_hit_cache,
                    &state.artifacts,
                    state.in_flight_bytes.load(Ordering::Relaxed),
                );
                if current.total_bytes as u64 <= plan.target_bytes() {
                    drop(dep_graph_guard);
                    return (total_freed, total_items);
                }
                let next = super::super::eviction::plan_eviction_to_target_excluding(
                    plan.target_bytes(),
                    &state.cache_system,
                    &dep_graph_guard,
                    &state.fast_hit_cache,
                    &state.artifacts,
                    state.in_flight_bytes.load(Ordering::Relaxed),
                    &protected_paths,
                );
                drop(dep_graph_guard);
                let Some(next) = next else {
                    return (total_freed, total_items);
                };
                if current.metadata_entries > 0 && next.metadata_candidate_count() == 0 {
                    return (total_freed, total_items);
                }
                if forced_completion_stalled(
                    outcome.items_removed,
                    completion_had_candidates,
                    current.metadata_entries,
                ) {
                    // Remaining pressure comes only from non-evictable state
                    // (for example in-flight persistence bytes, artifact-map
                    // overhead, or intentionally preserved recent fast hits).
                    return (total_freed, total_items);
                }
                completion_plan = next;
            }
        }

        if started.elapsed() < MEMORY_GC_IDLE_GRACE {
            let winner = {
                let idle = state.cache_requests_idle.notified();
                let delay = kernal_api::async_engine::sleep(
                    MEMORY_GC_IDLE_GRACE.saturating_sub(started.elapsed()),
                );
                let mut idle = std::pin::pin!(idle);
                let mut delay = std::pin::pin!(delay);
                kernal_api::fair_race!((idle.as_mut()), (delay.as_mut())).await
            };
            match winner {
                kernal_api::async_engine::FairRace2::First(())
                | kernal_api::async_engine::FairRace2::Second(()) => {}
            }
            continue;
        }

        if !gentle_finished {
            let outcome = super::super::eviction::try_evict_metadata_plan(
                &state.cache_system,
                &plan,
                candidate_offset,
                MEMORY_GC_GENTLE_BATCH,
            );
            total_freed += outcome.freed_bytes;
            total_items += outcome.items_removed;
            busy_candidates.extend(outcome.busy);
            protected_paths.extend(
                outcome
                    .refreshed
                    .iter()
                    .map(|candidate| candidate.path().clone()),
            );
            candidate_offset = candidate_offset.saturating_add(MEMORY_GC_GENTLE_BATCH);

            let dep_graph_guard = state.dep_graph.load();
            let current = super::super::eviction::memory_snapshot(
                &state.cache_system,
                &dep_graph_guard,
                &state.fast_hit_cache,
                &state.artifacts,
                state.in_flight_bytes.load(Ordering::Relaxed),
            );
            drop(dep_graph_guard);
            gentle_finished = current.total_bytes as u64 <= plan.target_bytes()
                || candidate_offset >= plan.metadata_candidate_count();
        }

        let winner = {
            let idle = state.cache_requests_idle.notified();
            let delay = kernal_api::async_engine::sleep(MEMORY_GC_GENTLE_RETRY);
            let mut idle = std::pin::pin!(idle);
            let mut delay = std::pin::pin!(delay);
            kernal_api::fair_race!((idle.as_mut()), (delay.as_mut())).await
        };
        match winner {
            kernal_api::async_engine::FairRace2::First(())
            | kernal_api::async_engine::FairRace2::Second(()) => {}
        }
    }
}

fn forced_completion_stalled(
    items_removed: usize,
    completion_had_candidates: bool,
    metadata_entries: usize,
) -> bool {
    items_removed == 0 && !completion_had_candidates && metadata_entries == 0
}

#[path = "run_artifact_loader.rs"]
mod artifact_loader;
pub(super) use artifact_loader::spawn_artifact_loader;

#[cfg(test)]
mod memory_eviction_controller_tests {
    use super::forced_completion_stalled;

    #[test]
    fn in_flight_only_pressure_terminates_after_no_progress() {
        assert!(forced_completion_stalled(0, false, 0));
        assert!(!forced_completion_stalled(1, false, 0));
        assert!(!forced_completion_stalled(0, true, 0));
        assert!(!forced_completion_stalled(0, false, 1));
    }
}

#[cfg(test)]
#[path = "run_accept_tests.rs"]
mod accept_watchdog_tests;
