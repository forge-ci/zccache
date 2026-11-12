//! Batch artifact-index deltas into checksummed per-entry disk commits.

use super::*;
use crate::core::cache_root_lock::CacheRootWriterLease;
use std::io;

pub(super) enum IndexWriterCommand {
    Insert(String, ArtifactIndex),
    /// Refresh retention age without replacing newer artifact metadata.
    Touch(String, u64),
    Remove(Vec<String>),
    ClearBlocking(IndexClearReceiver),
    Flush(kernal_api::async_engine::OneshotSender<bool>),
    /// Blocking startup reconciliation needs proof that queued rows are committed.
    FlushForReconciliation(std::sync::mpsc::SyncSender<bool>),
}

/// Commit queued entries every 100 ms by default. Each entry is an atomic,
/// checksummed KV publication; a killed process loses only uncommitted work.
/// The in-memory lookup path performs no additional filesystem operations.
/// Override with `ZCCACHE_WAL_FLUSH_MS`.
pub(super) fn wal_flush_interval() -> std::time::Duration {
    let ms: u64 = std::env::var("ZCCACHE_WAL_FLUSH_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    std::time::Duration::from_millis(ms.max(1))
}

/// Maximum queued entries before committing early, independent of total store size.
pub(super) fn wal_max_pending() -> usize {
    std::env::var("ZCCACHE_WAL_MAX_PENDING")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2048)
        .max(1)
}

/// Background index-writer task.
///
/// Acts as an in-memory WAL in front of the on-disk bincode blob:
///   * persist tasks push `(key, ArtifactIndex)` into the channel; they don't
///     wait for the disk write (cheap send).
///   * this task drains the channel into an in-memory `HashMap` (the WAL),
///     dedup'ing repeat keys.
///   * the WAL is flushed to disk on a timer (`ZCCACHE_WAL_FLUSH_MS`, default
///     5 s) or eagerly when it exceeds a size budget
///     (`ZCCACHE_WAL_MAX_PENDING`, default 2048).
///   * each flush applies the batch to `ArtifactStore` (in-memory DashMap)
///     and then snapshots the whole map atomically via `ArtifactStore::flush`
///     (tmp file + rename). One sequential write per flush window.
///   * channel close signals a final flush + clean exit (used by graceful
///     shutdown).
///
/// Reads don't consult the WAL: the daemon's authoritative in-memory state
/// lives in `state.artifacts` (a `DashMap` populated synchronously by the
/// persist call-sites themselves), and the on-disk blob is consulted only at
/// startup via `load_all()`. Entries that haven't yet flushed are still
/// visible to the running daemon; they're just at risk of being lost across
/// an abrupt crash (where the files-on-disk are durable but the next
/// session's `load_all()` won't see them, forcing a one-time re-miss).
pub(super) async fn run_index_writer(
    mut rx: kernal_api::async_engine::UnboundedReceiver<IndexWriterCommand>,
    store: Arc<ArtifactStore>,
    shutdown: Arc<kernal_api::async_engine::Notify>,
    root: CacheRootWriterLease,
) {
    use std::collections::HashMap;
    // The actor owns its service claim; each accepted write gets an independent
    // strong reference before launching, so cancelling the actor cannot release
    // the native claim while a detached blocking write is still executing.
    let root = Arc::new(root);
    let flush_interval = wal_flush_interval();
    let max_pending = wal_max_pending();
    let mut wal: HashMap<String, ArtifactIndex> = HashMap::with_capacity(max_pending);
    let mut ticker = kernal_api::async_engine::PeriodicTimer::new_unbounded(flush_interval)
        .expect("WAL flush interval must be valid inside the daemon runtime");
    // Don't immediately fire on the first tick — wait one interval.
    ticker.set_missed_tick_behavior(kernal_api::async_engine::MissedTickBehavior::Delay);
    ticker.tick().await;

    // Retain the registered shutdown edge across idle receive timeouts and
    // timer wins; recreating this waiter can lose notify_waiters delivery.
    let shutdown_wait = shutdown.notified();
    let mut shutdown_wait = std::pin::pin!(shutdown_wait);
    shutdown_wait.as_mut().enable();
    loop {
        let winner = {
            let message = rx.recv_within(zccache_core::wait::LIFECYCLE_POLL);
            let tick = ticker.tick();
            let mut message = std::pin::pin!(message);
            let mut tick = std::pin::pin!(tick);
            kernal_api::fair_race!(
                (message.as_mut()),
                (tick.as_mut()),
                (shutdown_wait.as_mut()),
            )
            .await
        };
        match winner {
            kernal_api::async_engine::FairRace3::First(msg) => {
                match msg {
                    Ok(command) => {
                        process_index_writer_command(
                            command,
                            &store,
                            &mut wal,
                            max_pending,
                            &root,
                            &mut rx,
                        )
                        .await;
                        // Drain whatever else is already queued in this tick.
                        while let Ok(command) = rx.try_recv() {
                            process_index_writer_command(
                                command,
                                &store,
                                &mut wal,
                                max_pending,
                                &root,
                                &mut rx,
                            )
                            .await;
                        }
                    }
                    Err(kernal_api::async_engine::RecvTimeoutError::TimedOut) => continue,
                    Err(kernal_api::async_engine::RecvTimeoutError::Disconnected) => {
                        // Channel closed (last sender dropped). Final flush.
                        flush_wal_to_disk(&store, &mut wal, &root).await;
                        return;
                    }
                }
            }
            kernal_api::async_engine::FairRace3::Second(()) => {
                if !wal.is_empty() || store.has_pending_entries() || store.needs_checkpoint() {
                    flush_wal_to_disk(&store, &mut wal, &root).await;
                }
            }
            kernal_api::async_engine::FairRace3::Third(()) => {
                // Daemon-initiated graceful shutdown. Drain anything still
                // queued and flush before the runtime aborts us.
                while let Ok(command) = rx.try_recv() {
                    process_index_writer_command(
                        command,
                        &store,
                        &mut wal,
                        max_pending,
                        &root,
                        &mut rx,
                    )
                    .await;
                }
                tracing::info!(
                    pending = wal.len(),
                    "index-writer shutdown signal received, draining and flushing"
                );
                flush_wal_to_disk(&store, &mut wal, &root).await;
                return;
            }
        }
    }
}

async fn process_index_writer_command(
    command: IndexWriterCommand,
    store: &Arc<ArtifactStore>,
    wal: &mut std::collections::HashMap<String, ArtifactIndex>,
    max_pending: usize,
    root: &Arc<CacheRootWriterLease>,
    rx: &mut kernal_api::async_engine::UnboundedReceiver<IndexWriterCommand>,
) {
    match command {
        IndexWriterCommand::Insert(k, v) => {
            wal.insert(k, v);
            if wal.len() >= max_pending {
                flush_wal_to_disk(store, wal, root).await;
            }
        }
        IndexWriterCommand::Touch(k, stored_at_secs) => {
            if let Some(meta) = wal.get_mut(&k) {
                meta.stored_at_secs = meta.stored_at_secs.max(stored_at_secs);
            } else if let Some(mut meta) = store.get(&k) {
                meta.stored_at_secs = meta.stored_at_secs.max(stored_at_secs);
                wal.insert(k, meta);
            }
            if wal.len() >= max_pending {
                flush_wal_to_disk(store, wal, root).await;
            }
        }
        IndexWriterCommand::Remove(keys) => {
            for key in &keys {
                wal.remove(key);
            }
            let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
            let removed = store.remove_batch(&refs);
            match launch_index_flush(store, root)
                .await
                .unwrap_or_else(|error| Err(io::Error::other(error)))
            {
                Ok(()) => tracing::info!(removed, "artifact-index removals flushed to disk"),
                Err(error) => {
                    tracing::warn!(removed, %error, "artifact-index removal flush failed")
                }
            }
        }
        IndexWriterCommand::ClearBlocking(receiver) => {
            // Reserve the actor before Clear acquires publication. In particular,
            // any previous disk flush finishes before ready is acknowledged.
            let Some((publication, ordered, flushed)) = receiver
                .receive_for(OPERATION_TIMEOUT.saturating_mul(2))
                .await
            else {
                return;
            };
            // Clear now owns publication. Everything already queued belongs to
            // the old namespace; no publisher can enqueue a post-Clear command
            // until the ordered acknowledgement below releases that exclusion.
            let mut flush_acks = Vec::new();
            let mut reconciliation_acks = Vec::new();
            while let Ok(command) = rx.try_recv() {
                match command {
                    IndexWriterCommand::Flush(ack) => flush_acks.push(ack),
                    IndexWriterCommand::FlushForReconciliation(ack) => {
                        reconciliation_acks.push(ack)
                    }
                    _ => {}
                }
            }
            wal.clear();
            store.clear();
            // All older index commands have been retired. The publication
            // commit may end now; disk completion has its own acknowledgement.
            drop(publication);
            let _ = ordered.try_send(());
            let result = launch_index_flush(store, root)
                .await
                .unwrap_or_else(|error| Err(io::Error::other(error)));
            if let Err(error) = &result {
                tracing::warn!(%error, "cleared artifact index flush failed");
            }
            let _ = flushed.try_send(result.is_ok());
            for ack in reconciliation_acks {
                let _ = ack.try_send(result.is_ok());
            }
            for ack in flush_acks {
                let _ = ack.send(result.is_ok());
            }
        }
        IndexWriterCommand::Flush(ack) => {
            let committed = flush_wal_to_disk(store, wal, root).await;
            let _ = ack.send(committed);
        }
        IndexWriterCommand::FlushForReconciliation(ack) => {
            let committed = flush_wal_to_disk(store, wal, root).await;
            let _ = ack.try_send(committed);
        }
    }
}

// Nonblocking construction: the service owns the returned, cancellation-safe
// job. Its request acknowledgements have separate explicit wait budgets.
fn launch_index_flush(
    store: &Arc<ArtifactStore>,
    root: &Arc<CacheRootWriterLease>,
) -> kernal_api::async_engine::Task<io::Result<()>> {
    let store = Arc::clone(store);
    let root = Arc::clone(root);
    kernal_api::async_engine::launch_blocking(move || {
        // Root is captured last, then bound first: both queued cancellation
        // and normal/unwind teardown release the store before native ownership.
        let _ = &store;
        let _root = root;
        let writer_store = store;
        #[cfg(test)]
        persist::StagedHookGuard::pause_index_flush(&writer_store);
        if writer_store.needs_checkpoint() {
            writer_store.flush()
        } else {
            writer_store.flush_incremental()
        }
    })
    .detach_on_drop()
}

pub(super) async fn flush_index_writer(
    tx: &kernal_api::async_engine::UnboundedSender<IndexWriterCommand>,
    timeout: std::time::Duration,
) -> bool {
    flush_index_writer_result(tx, timeout).await.is_ok()
}

pub(super) async fn flush_index_writer_result(
    tx: &kernal_api::async_engine::UnboundedSender<IndexWriterCommand>,
    timeout: std::time::Duration,
) -> Result<(), shutdown_completion::ShutdownError> {
    use shutdown_completion::ShutdownError;
    let (ack_tx, ack_rx) = kernal_api::async_engine::oneshot_channel();
    tx.send(IndexWriterCommand::Flush(ack_tx))
        .map_err(|_| ShutdownError::Persistence)?;
    match kernal_api::async_engine::timeout(timeout, ack_rx).await {
        Ok(Ok(true)) => Ok(()),
        Ok(_) => Err(ShutdownError::Persistence),
        Err(_) => Err(ShutdownError::BudgetExceeded),
    }
}

pub(super) struct PendingIndexClear {
    flushed: std::sync::mpsc::Receiver<bool>,
}

/// Sealed transfer state; only the reserved sender constructs this value.
/// Dylint checks construction, unsealing and the receive owner's lack of I/O.
pub(super) struct IndexClearCommit {
    publication: crate::core::sync::PublicationCommitPermit,
}

impl IndexClearCommit {
    fn new(publication: crate::core::sync::PublicationCommitPermit) -> Self {
        Self { publication }
    }

    fn into_publication(self) -> crate::core::sync::PublicationCommitPermit {
        self.publication
    }
}

pub(super) struct IndexClearReceiver {
    ready: std::sync::mpsc::SyncSender<()>,
    commit: kernal_api::async_engine::OneshotReceiver<IndexClearCommit>,
    ordered: std::sync::mpsc::SyncSender<()>,
    flushed: std::sync::mpsc::SyncSender<bool>,
    #[cfg(test)]
    receive_gate: Option<Arc<Notify>>,
}

impl IndexClearReceiver {
    async fn receive_for(
        self,
        bound: Duration,
    ) -> Option<(
        crate::core::sync::PublicationCommitPermit,
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::SyncSender<bool>,
    )> {
        // Before ready, no sender can possess a reserved commit capability.
        // After ready, this sealed owner only waits and immediately unseals.
        self.ready.try_send(()).ok()?;
        #[cfg(test)]
        if let Some(gate) = &self.receive_gate {
            kernal_api::async_engine::timeout(bound, gate.notified())
                .await
                .ok()?;
        }
        let token = kernal_api::async_engine::timeout(bound, self.commit)
            .await
            .ok()?
            .ok()?;
        Some((token.into_publication(), self.ordered, self.flushed))
    }
}

pub(super) struct ReservedIndexClear {
    commit: kernal_api::async_engine::OneshotSender<IndexClearCommit>,
    ordered: std::sync::mpsc::Receiver<()>,
    flushed: std::sync::mpsc::Receiver<bool>,
}

impl ReservedIndexClear {
    /// The reserved actor performs only memory work before acknowledging this
    /// commit; no earlier or current disk operation is awaited under exclusion.
    pub(super) fn commit_for(
        self,
        publication: crate::core::sync::PublicationCommitPermit,
        timeout: Duration,
    ) -> Option<PendingIndexClear> {
        // The actor owns exclusion even if this waiter times out or unwinds;
        // it can never apply a late Clear to a newer publication.
        self.commit.send(IndexClearCommit::new(publication)).ok()?;
        crate::core::wait::recv_for(&self.ordered, timeout).ok()?;
        Some(PendingIndexClear {
            flushed: self.flushed,
        })
    }
}

impl PendingIndexClear {
    pub(super) fn wait_for(self, timeout: Duration) -> bool {
        matches!(
            crate::core::wait::recv_for(&self.flushed, timeout),
            Ok(true)
        )
    }
}

pub(super) fn reserve_index_clear_blocking(
    tx: &kernal_api::async_engine::UnboundedSender<IndexWriterCommand>,
    timeout: std::time::Duration,
) -> Option<ReservedIndexClear> {
    let (ready, ready_rx) = std::sync::mpsc::sync_channel(1);
    let (commit, commit_rx) = kernal_api::async_engine::oneshot_channel();
    let (ordered_tx, ordered_rx) = std::sync::mpsc::sync_channel(1);
    let (flushed, flushed_rx) = std::sync::mpsc::sync_channel(1);
    tx.send(IndexWriterCommand::ClearBlocking(IndexClearReceiver {
        ready,
        commit: commit_rx,
        ordered: ordered_tx,
        flushed,
        #[cfg(test)]
        receive_gate: None,
    }))
    .ok()?;
    crate::core::wait::recv_for(&ready_rx, timeout).ok()?;
    Some(ReservedIndexClear {
        commit,
        ordered: ordered_rx,
        flushed: flushed_rx,
    })
}

#[cfg(test)]
fn clear_index_writer_blocking(
    tx: &kernal_api::async_engine::UnboundedSender<IndexWriterCommand>,
    timeout: Duration,
) -> bool {
    reserve_index_clear_blocking(tx, crate::core::wait::PROBE)
        .and_then(|reserved| {
            reserved.commit_for(
                crate::core::sync::PublicationBarrier::new()
                    .blocking_write_for(crate::core::wait::PROBE),
                crate::core::wait::PROBE,
            )
        })
        .is_some_and(|pending| pending.wait_for(timeout))
}

pub(super) async fn flush_wal_to_disk(
    store: &Arc<ArtifactStore>,
    wal: &mut std::collections::HashMap<String, ArtifactIndex>,
    root: &Arc<CacheRootWriterLease>,
) -> bool {
    if wal.is_empty() && !store.has_pending_entries() && !store.needs_checkpoint() {
        return true;
    }
    let drained: Vec<(String, ArtifactIndex)> = wal.drain().collect();
    let count = drained.len();
    // Apply the batch to the in-memory store synchronously (cheap), then
    // do the disk write off the runtime thread so the flush doesn't block
    // request handlers.
    store.insert_many(drained);
    let res = launch_index_flush(store, root)
        .await
        .unwrap_or_else(|error| Err(io::Error::other(error)));
    match res {
        Ok(()) => {
            tracing::info!(committed = count, "WAL flushed to disk");
            true
        }
        Err(e) => {
            tracing::warn!(count, "WAL flush to disk failed: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_receive_releases_a_queued_clear_permit_without_acknowledging_reset() {
        crate::test_support::test_timeout(async {
            let barrier = crate::core::sync::PublicationBarrier::new();
            let (ready, ready_rx) = std::sync::mpsc::sync_channel(1);
            let (commit, commit_rx) = kernal_api::async_engine::oneshot_channel();
            let (ordered, ordered_rx) = std::sync::mpsc::sync_channel(1);
            let (flushed, flushed_rx) = std::sync::mpsc::sync_channel(1);
            let receiver = IndexClearReceiver {
                ready,
                commit: commit_rx,
                ordered,
                flushed,
                receive_gate: Some(Arc::new(Notify::new())),
            };
            let receiving = tokio::spawn(receiver.receive_for(Duration::from_secs(5)));
            tokio::task::spawn_blocking(move || {
                crate::core::wait::recv_for(&ready_rx, Duration::from_secs(5)).unwrap();
            })
            .await
            .unwrap();
            let publication = barrier.blocking_write_for(Duration::from_secs(5));
            assert!(commit.send(IndexClearCommit::new(publication)).is_ok());
            assert!(
                barrier.try_read_owned().is_err(),
                "queued commit lost exclusion"
            );
            receiving.abort();
            let cancelled = tokio::time::timeout(Duration::from_secs(5), receiving)
                .await
                .unwrap();
            assert!(matches!(cancelled, Err(error) if error.is_cancelled()));
            assert!(
                barrier.try_read_owned().is_ok(),
                "cancelled receive leaked exclusion"
            );
            assert!(matches!(
                ordered_rx.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Disconnected)
            ));
            assert!(matches!(
                flushed_rx.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Disconnected)
            ));
        })
        .await;
    }

    #[tokio::test]
    async fn cancelled_index_writer_keeps_root_until_timed_out_clear_write_finishes() {
        crate::test_support::test_timeout(async {
            let directory = tempfile::tempdir().unwrap();
            let cache_dir = NormalizedPath::new(directory.path());
            let endpoint = crate::ipc::unique_test_endpoint();
            let mut server = DaemonServer::bind_with_cache_dir(&endpoint, &cache_dir).unwrap();
            // Seed a durable old row so reopening after the incremental Clear
            // proves its journal checkpoint supersedes the legacy index.
            server.state.artifact_store.insert(
                "old",
                &ArtifactIndex::new(vec!["old.o".into()], vec![1], vec![], vec![], 0),
            );
            server.state.artifact_store.flush().unwrap();
            let hook = persist::StagedHookGuard::arm_index_flush(&server.state.artifact_store);
            let writer = tokio::spawn(run_index_writer(
                server.index_writer_rx.take().unwrap(),
                Arc::clone(&server.state.artifact_store),
                Arc::clone(&server.state.index_writer_shutdown),
                server
                    .state
                    .cache_root_lock
                    .lease_for(STATE_TIMEOUT)
                    .unwrap(),
            ));
            let tx = server.state.index_writer_tx.clone();
            let clear = tokio::task::spawn_blocking(move || {
                clear_index_writer_blocking(&tx, Duration::from_millis(5))
            });
            let hook = tokio::task::spawn_blocking(move || {
                hook.wait_until_reached_for(Duration::from_secs(10));
                hook
            })
            .await
            .unwrap();
            let acknowledged = tokio::time::timeout(Duration::from_secs(5), clear)
                .await
                .unwrap()
                .unwrap();
            writer.abort();
            let cancelled = tokio::time::timeout(Duration::from_secs(5), writer)
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled();
            server.state.cache_root_lock.release();
            let path = directory.path().to_path_buf();
            let excluded = tokio::task::spawn_blocking(move || {
                match crate::core::cache_root_lock::CacheRootWriterLock::acquire(&path) {
                    Err(error) => error.kind() == io::ErrorKind::WouldBlock,
                    Ok(contender) => {
                        drop(contender);
                        false
                    }
                }
            })
            .await
            .unwrap();
            hook.resume();
            // Root reuse, rather than the legacy index.bin rename, observes
            // completion of either a journal commit or a full checkpoint.
            let path = directory.path().to_path_buf();
            let next = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let path = path.clone();
                    match tokio::task::spawn_blocking(move || {
                        crate::core::cache_root_lock::CacheRootWriterLock::acquire(&path)
                    })
                    .await
                    .unwrap()
                    {
                        Ok(next) => break next,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        Err(error) => panic!("root reuse failed: {error}"),
                    }
                }
            })
            .await
            .expect("finished WAL write must release native ownership");
            let reopened = ArtifactStore::open(
                crate::core::config::index_path_from_cache_dir(&cache_dir).as_path(),
            )
            .unwrap();
            assert!(
                reopened.get("old").is_none(),
                "Clear must durably supersede the old row"
            );
            drop(next);
            assert!(
                !acknowledged,
                "Clear must report the acknowledgement timeout"
            );
            assert!(
                cancelled,
                "the actor waiter must cancel while disk work is paused"
            );
            assert!(
                excluded,
                "the detached WAL write lost native root ownership"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn wal_timer_accepts_a_legacy_long_interval_without_env_mutation() {
        let mut timer = kernal_api::async_engine::PeriodicTimer::new_unbounded(
            std::time::Duration::from_secs(366 * 24 * 60 * 60),
        )
        .expect("legacy WAL intervals above the policy-bounded timer limit remain valid");
        timer.set_missed_tick_behavior(kernal_api::async_engine::MissedTickBehavior::Delay);
    }

    #[tokio::test]
    async fn access_touch_merges_into_newest_pending_verdict_row() {
        let temp = tempfile::tempdir().expect("temporary index directory");
        let controller =
            crate::core::cache_root_lock::CacheRootWriterLock::acquire(temp.path()).unwrap();
        let root = Arc::new(controller.lease_for(STATE_TIMEOUT).unwrap());
        let store = Arc::new(ArtifactStore::open_empty(&temp.path().join("index.bin")));
        let mut wal = std::collections::HashMap::new();
        let (_tx, mut rx) = kernal_api::async_engine::unbounded_channel();
        let key = "b".repeat(64);
        let mut stale = ArtifactIndex::new(
            vec!["output.rmeta".to_string()],
            vec![4096],
            Vec::new(),
            Vec::new(),
            0,
        );
        stale.stored_at_secs = 10;
        store.insert(&key, &stale);

        let mut newest = stale;
        newest.rustc_verdicts.insert(
            "dylint".to_string(),
            ArtifactVerdict {
                stdout: Arc::new(Vec::new()),
                stderr: Arc::new(b"lint verdict".to_vec()),
                exit_code: 1,
            },
        );
        process_index_writer_command(
            IndexWriterCommand::Insert(key.clone(), newest),
            &store,
            &mut wal,
            usize::MAX,
            &root,
            &mut rx,
        )
        .await;
        process_index_writer_command(
            IndexWriterCommand::Touch(key.clone(), 20),
            &store,
            &mut wal,
            usize::MAX,
            &root,
            &mut rx,
        )
        .await;

        let pending = wal.get(&key).unwrap();
        assert_eq!(pending.stored_at_secs, 20);
        assert_eq!(pending.rustc_verdicts.len(), 1);
        assert_eq!(pending.output_names.as_ref(), &["output.rmeta"]);
    }

    #[tokio::test]
    async fn remove_cancels_pending_insert_and_persists_deletion() {
        let temp = tempfile::tempdir().expect("temporary index directory");
        let controller =
            crate::core::cache_root_lock::CacheRootWriterLock::acquire(temp.path()).unwrap();
        let root = Arc::new(controller.lease_for(STATE_TIMEOUT).unwrap());
        let index_path = temp.path().join("index.bin");
        let store = Arc::new(ArtifactStore::open_empty(&index_path));
        let mut wal = std::collections::HashMap::new();
        let (_tx, mut rx) = kernal_api::async_engine::unbounded_channel();
        let key = "a".repeat(64);
        let meta = ArtifactIndex::new(
            vec!["output.o".to_string()],
            vec![4096],
            Vec::new(),
            Vec::new(),
            0,
        );

        process_index_writer_command(
            IndexWriterCommand::Insert(key.clone(), meta),
            &store,
            &mut wal,
            usize::MAX,
            &root,
            &mut rx,
        )
        .await;
        assert!(wal.contains_key(&key));

        process_index_writer_command(
            IndexWriterCommand::Remove(vec![key.clone()]),
            &store,
            &mut wal,
            usize::MAX,
            &root,
            &mut rx,
        )
        .await;

        assert!(!wal.contains_key(&key));
        assert!(store.get(&key).is_none());
        let reopened = ArtifactStore::open(&index_path).expect("reopen persisted index");
        assert!(reopened.get(&key).is_none());
    }
}
