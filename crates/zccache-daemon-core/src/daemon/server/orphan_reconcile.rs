//! Startup repair of payloads left behind before an index commit.

use super::persist::{DetachedStagedTrash, PinnedName};
use super::{IndexWriterCommand, SharedState};
use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

fn known(state: &SharedState, key: &str) -> bool {
    state.artifacts.contains_key(key) || state.artifact_store.get(key).is_some()
}

fn legacy_orphans(
    state: &SharedState,
    started: Instant,
    budget: Duration,
) -> io::Result<Vec<(String, PinnedName)>> {
    let mut orphans = Vec::new();
    let entries = match std::fs::read_dir(&state.artifact_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(orphans),
        Err(error) => return Err(error),
    };
    for entry in entries {
        if started.elapsed() >= budget {
            break;
        }
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some((key, _)) = name.to_str().and_then(super::disk_maintenance::legacy_key) else {
            continue;
        };
        if !known(state, key) {
            match PinnedName::open_owned(state.artifact_dir.as_path(), &entry.path()) {
                Ok(pin) => orphans.push((key.to_string(), pin)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(orphans)
}

fn flush_queued_index(state: &SharedState, bound: Duration) -> io::Result<()> {
    let (ack, receiver) = std::sync::mpsc::sync_channel(1);
    state
        .index_writer_tx
        .send(IndexWriterCommand::FlushForReconciliation(ack))
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "index writer unavailable during orphan reconciliation",
            )
        })?;
    match crate::core::wait::recv_for(&receiver, bound) {
        Ok(true) => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "index commits not confirmed; orphan deletion deferred",
        )),
    }
}

struct OrphanPlan {
    staged: HashMap<String, (Option<String>, PinnedName)>,
    legacy: Vec<(String, PinnedName)>,
    outcome: crate::artifact::IndexReconciliation,
}

fn unchanged(pin: &PinnedName) -> io::Result<bool> {
    match pin.unchanged() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        result => result,
    }
}

fn prepare(state: &SharedState, started: Instant, budget: Duration) -> io::Result<OrphanPlan> {
    // Readers protect this verification from Clear and generation retirement;
    // publishers can still run. Pinned pointer identities bind verification to
    // the captured generation even when a publisher replaces its pointer.
    let _publication = state.artifact_publication.blocking_read_for(budget);
    let _store = crate::artifact::StagedReadGuard::acquire_for(
        state.artifact_dir.as_path(),
        budget.saturating_sub(started.elapsed()),
    )?;
    let mut staged = HashMap::new();
    for entry in super::persist::scan_v2_disk_artifacts(state.artifact_dir.as_path())? {
        if started.elapsed() >= budget {
            break;
        }
        if known(state, &entry.key) || entry.generation.is_none() {
            continue;
        }
        let path = super::persist::staged_generation_pointer_path(
            state.artifact_dir.as_path(),
            &entry.key,
        );
        let pin = match PinnedName::open_owned(state.artifact_dir.as_path(), path.as_path()) {
            Ok(pin) => pin,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let current = super::persist::read_staged_generation_pointer_for(
            path.as_path(),
            budget.saturating_sub(started.elapsed()),
        )?;
        if entry.generation.as_deref() == Some(current.as_str()) && unchanged(&pin)? {
            staged.insert(entry.key, (entry.generation, pin));
        }
    }
    let legacy = legacy_orphans(state, started, budget)?;
    let outcome = crate::artifact::reconcile_missing_index_entries(
        state.artifact_dir.as_path(),
        budget.saturating_sub(started.elapsed()),
        |key| known(state, key) || !staged.contains_key(key),
    );
    Ok(OrphanPlan {
        staged,
        legacy,
        outcome,
    })
}

fn commit(
    state: &SharedState,
    plan: &OrphanPlan,
    started: Instant,
    budget: Duration,
) -> io::Result<(
    usize,
    super::persist::DetachedStagedEviction,
    DetachedStagedTrash,
)> {
    let _publication = state
        .artifact_publication
        .blocking_write_for(budget.saturating_sub(started.elapsed()));
    let mut recovered = 0;
    for (key, meta) in &plan.outcome.entries {
        if started.elapsed() >= budget {
            break;
        }
        if !known(state, key)
            && plan
                .staged
                .get(key)
                .is_some_and(|(_, pin)| unchanged(pin).unwrap_or(false))
        {
            state.artifact_store.insert(key, meta);
            recovered += 1;
        }
    }
    let mut deletions = HashMap::new();
    for key in &plan.outcome.unrecoverable_keys {
        if started.elapsed() >= budget {
            break;
        }
        if let Some((generation, pin)) = plan.staged.get(key) {
            if !known(state, key) && unchanged(pin)? {
                deletions.insert(key.clone(), generation.clone());
            }
        }
    }
    let mut detached = super::persist::detach_staged_artifact_keys_if_unchanged(
        state.artifact_dir.as_path(),
        &deletions,
        budget.saturating_sub(started.elapsed()),
    )?;
    let mut legacy_trash = DetachedStagedTrash::empty(state.artifact_dir.as_path());
    for (key, pin) in &plan.legacy {
        if started.elapsed() >= budget {
            break;
        }
        if !known(state, key) && unchanged(pin)? {
            legacy_trash
                .detach_legacy_blob_for(pin.path(), budget.saturating_sub(started.elapsed()))?;
            detached.removed.insert(key.clone());
        }
    }
    state
        .dep_graph
        .load()
        .invalidate_artifact_keys(&detached.removed);
    Ok((recovered, detached, legacy_trash))
}

/// Capture and verify without excluding publishers, then commit only unchanged
/// identities. Never wait for the index actor while holding publication: Clear
/// reserves that actor before acquiring publication in the opposite direction.
pub(super) fn reconcile(state: &SharedState, budget: Duration) -> io::Result<(usize, usize)> {
    if budget.is_zero() {
        return Ok((0, 0));
    }
    let started = Instant::now();
    let _root = state.cache_root_lock.lease_for(budget).ok_or_else(|| {
        io::Error::other("cache root owner unavailable during orphan reconciliation")
    })?;
    let plan = prepare(state, started, budget)?;
    if plan.staged.is_empty() && plan.legacy.is_empty() {
        return Ok((0, 0));
    }
    #[cfg(test)]
    super::persist::staged_hook_pause(
        state.artifact_dir.as_path(),
        super::persist::StagedHookPoint::OrphanPlanReady,
    );
    flush_queued_index(state, budget.saturating_sub(started.elapsed()))?;
    let (recovered, detached, legacy_trash) = commit(state, &plan, started, budget)?;
    let truncated = plan.outcome.truncated_by_budget;
    // Windows also requires the pinned file handles to close before sweeping.
    drop(plan);
    detached
        .trash
        .sweep_for(budget.saturating_sub(started.elapsed()))?;
    legacy_trash.sweep_for(budget.saturating_sub(started.elapsed()))?;
    // Index checkpoints and journal reclamation own their persistence gate and
    // cache-root lease, and must run after publication exclusion has ended.
    state.artifact_store.flush_incremental()?;
    if recovered > 0 || !detached.removed.is_empty() {
        crate::core::lifecycle::write_event_in_cache_root(
            state.cache_dir.as_path(),
            crate::core::lifecycle::EVENT_INDEX_RECONCILED,
            serde_json::json!({ "reason": "startup_orphans", "recovered": recovered, "removed": detached.removed.len(), "truncated_by_budget": truncated }),
        );
    }
    Ok((recovered, detached.removed.len()))
}

#[cfg(test)]
mod tests {
    use super::super::{run_index_writer, DaemonServer};
    use super::*;
    use crate::artifact::layout_fixtures::seed_staged_generation;
    use std::sync::Arc;

    #[tokio::test]
    async fn queued_index_metadata_protects_a_recently_published_flat_payload() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::core::NormalizedPath::new(temp.path());
        let mut server =
            DaemonServer::bind_with_cache_dir(&crate::ipc::unique_test_endpoint(), &root).unwrap();
        let key = "d".repeat(64);
        let payload = server.state.artifact_dir.join(format!("{key}_0"));
        std::fs::write(&payload, b"object").unwrap();
        server
            .state
            .index_writer_tx
            .send(IndexWriterCommand::Insert(
                key.clone(),
                crate::artifact::ArtifactIndex::new(
                    vec!["queued.o".into()],
                    vec![6],
                    vec![],
                    vec![],
                    0,
                ),
            ))
            .unwrap_or_else(|_| panic!("index writer receiver must remain connected"));
        let writer = kernal_api::async_engine::launch(run_index_writer(
            server.index_writer_rx.take().unwrap(),
            Arc::clone(&server.state.artifact_store),
            Arc::clone(&server.state.index_writer_shutdown),
            server
                .state
                .cache_root_lock
                .lease_for(Duration::from_secs(5))
                .unwrap(),
        ));
        let state = Arc::clone(&server.state);
        let result = kernal_api::async_engine::launch_blocking(move || {
            reconcile(&state, Duration::from_secs(5))
        })
        .await
        .unwrap()
        .unwrap();
        writer.cancel();
        assert_eq!(result, (0, 0));
        assert!(payload.exists());
        assert!(server.state.artifact_store.get(&key).is_some());
    }

    #[tokio::test]
    async fn missing_index_writer_defers_deletion_instead_of_guessing() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::core::NormalizedPath::new(temp.path());
        let mut server =
            DaemonServer::bind_with_cache_dir(&crate::ipc::unique_test_endpoint(), &root).unwrap();
        drop(server.index_writer_rx.take());
        let payload = server
            .state
            .artifact_dir
            .join(format!("{}_0", "e".repeat(64)));
        std::fs::write(&payload, b"unindexed").unwrap();
        let state = Arc::clone(&server.state);
        let result = kernal_api::async_engine::launch_blocking(move || {
            reconcile(&state, Duration::from_secs(5))
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert!(
            payload.exists(),
            "unconfirmed index writes must prevent cleanup"
        );
    }

    #[tokio::test]
    async fn healthy_index_recovers_an_orphan_and_removes_unrecoverable_outputs() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::core::NormalizedPath::new(temp.path());
        let mut server =
            DaemonServer::bind_with_cache_dir(&crate::ipc::unique_test_endpoint(), &root).unwrap();
        let recoverable = "a".repeat(64);
        let unrecoverable = "b".repeat(64);
        seed_staged_generation(
            server.state.artifact_dir.as_path(),
            &recoverable,
            &[b"object"],
        );
        seed_staged_generation(
            server.state.artifact_dir.as_path(),
            &unrecoverable,
            &[b"object", b"depfile"],
        );
        let legacy = server
            .state
            .artifact_dir
            .join(format!("{}_0", "c".repeat(64)));
        std::fs::write(&legacy, b"unindexed flat output").unwrap();
        server.state.artifact_store.flush().unwrap();
        let writer = kernal_api::async_engine::launch(run_index_writer(
            server.index_writer_rx.take().unwrap(),
            Arc::clone(&server.state.artifact_store),
            Arc::clone(&server.state.index_writer_shutdown),
            server
                .state
                .cache_root_lock
                .lease_for(Duration::from_secs(5))
                .unwrap(),
        ));
        let state = Arc::clone(&server.state);
        let result = kernal_api::async_engine::launch_blocking(move || {
            reconcile(&state, Duration::from_secs(5))
        })
        .await
        .unwrap()
        .unwrap();
        writer.cancel();
        assert_eq!(result, (1, 2));
        assert!(server.state.artifact_store.get(&recoverable).is_some());
        assert!(!legacy.exists());
        let remaining =
            super::super::persist::scan_v2_disk_artifacts(server.state.artifact_dir.as_path())
                .unwrap();
        assert!(!remaining
            .iter()
            .any(|artifact| artifact.key == unrecoverable));
    }
    fn verified_sizes(state: &SharedState, key: &str) -> Vec<u64> {
        crate::artifact::reconcile_missing_index_entries(
            state.artifact_dir.as_path(),
            Duration::from_secs(5),
            |candidate| candidate != key,
        )
        .entries
        .into_iter()
        .next()
        .unwrap()
        .1
        .output_sizes
    }

    #[tokio::test]
    async fn waiting_for_index_acknowledgment_releases_publication() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::core::NormalizedPath::new(temp.path());
        let mut server =
            DaemonServer::bind_with_cache_dir(&crate::ipc::unique_test_endpoint(), &root).unwrap();
        let payload = server
            .state
            .artifact_dir
            .join(format!("{}_0", "f".repeat(64)));
        std::fs::write(&payload, b"orphan").unwrap();
        let mut receiver = server.index_writer_rx.take().unwrap();
        let state = Arc::clone(&server.state);
        let worker = kernal_api::async_engine::launch_blocking(move || {
            reconcile(&state, Duration::from_secs(10))
        });
        let command = receiver.recv_within(Duration::from_secs(5)).await.unwrap();
        let IndexWriterCommand::FlushForReconciliation(ack) = command else {
            panic!("expected reconciliation barrier");
        };
        // Clear must be able to acquire this before its reserved actor can
        // process the reconciliation acknowledgment.
        let publication_available = server.state.artifact_publication.try_write().is_some();
        ack.try_send(true).unwrap();
        let result = kernal_api::async_engine::timeout(Duration::from_secs(10), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            publication_available,
            "index acknowledgment wait retained publication exclusion"
        );
        assert_eq!(result, (0, 1));
    }

    #[tokio::test]
    async fn stale_orphan_plan_preserves_republished_generations() {
        use super::super::persist::{StagedHookGuard, StagedHookPoint};
        let temp = tempfile::tempdir().unwrap();
        let root = crate::core::NormalizedPath::new(temp.path());
        let mut server =
            DaemonServer::bind_with_cache_dir(&crate::ipc::unique_test_endpoint(), &root).unwrap();
        let single = "a".repeat(64);
        let multi = "b".repeat(64);
        seed_staged_generation(server.state.artifact_dir.as_path(), &single, &[b"old"]);
        seed_staged_generation(
            server.state.artifact_dir.as_path(),
            &multi,
            &[b"old", b"deps"],
        );
        let hook = StagedHookGuard::arm(
            server.state.artifact_dir.as_path(),
            StagedHookPoint::OrphanPlanReady,
        );
        let state = Arc::clone(&server.state);
        let worker = kernal_api::async_engine::launch_blocking(move || {
            reconcile(&state, Duration::from_secs(15))
        });
        let hook = kernal_api::async_engine::launch_blocking(move || {
            hook.wait_until_reached_for(Duration::from_secs(5));
            hook
        })
        .await
        .unwrap();
        {
            let _publication = server
                .state
                .artifact_publication
                .blocking_read_for(Duration::from_secs(2));
            seed_staged_generation(
                server.state.artifact_dir.as_path(),
                &single,
                &[b"replacement"],
            );
            seed_staged_generation(
                server.state.artifact_dir.as_path(),
                &multi,
                &[b"replacement"],
            );
        }
        let writer = kernal_api::async_engine::launch(run_index_writer(
            server.index_writer_rx.take().unwrap(),
            Arc::clone(&server.state.artifact_store),
            Arc::clone(&server.state.index_writer_shutdown),
            server
                .state
                .cache_root_lock
                .lease_for(Duration::from_secs(5))
                .unwrap(),
        ));
        hook.resume();
        let result = kernal_api::async_engine::timeout(Duration::from_secs(15), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        writer.cancel();
        assert_eq!(
            result,
            (0, 0),
            "stale verification must neither rebuild a row nor retire a new generation"
        );
        assert!(server.state.artifact_store.get(&single).is_none());
        for key in [&single, &multi] {
            assert_eq!(verified_sizes(&server.state, key), vec![11]);
        }
    }

    #[tokio::test]
    async fn detached_orphan_sweep_allows_publication_and_journal_compaction() {
        use super::super::persist::{StagedHookGuard, StagedHookPoint};
        let temp = tempfile::tempdir().unwrap();
        let root = crate::core::NormalizedPath::new(temp.path());
        let mut server =
            DaemonServer::bind_with_cache_dir(&crate::ipc::unique_test_endpoint(), &root).unwrap();
        let key = "c".repeat(64);
        seed_staged_generation(
            server.state.artifact_dir.as_path(),
            &key,
            &[b"old", b"deps"],
        );
        let writer = kernal_api::async_engine::launch(run_index_writer(
            server.index_writer_rx.take().unwrap(),
            Arc::clone(&server.state.artifact_store),
            Arc::clone(&server.state.index_writer_shutdown),
            server
                .state
                .cache_root_lock
                .lease_for(Duration::from_secs(5))
                .unwrap(),
        ));
        let hook = StagedHookGuard::arm(
            server.state.artifact_dir.as_path(),
            StagedHookPoint::TreeSweep,
        );
        let state = Arc::clone(&server.state);
        let worker = kernal_api::async_engine::launch_blocking(move || {
            reconcile(&state, Duration::from_secs(15))
        });
        let hook = kernal_api::async_engine::launch_blocking(move || {
            hook.wait_until_reached_for(Duration::from_secs(5));
            hook
        })
        .await
        .unwrap();
        let publication_available = server.state.artifact_publication.try_write().is_some();
        let native_available = crate::artifact::StagedReadGuard::acquire_for(
            server.state.artifact_dir.as_path(),
            Duration::ZERO,
        )
        .is_ok();
        // Resume even on the baseline failure path so the worker cannot strand.
        if !publication_available || !native_available {
            hook.resume();
            let _ = kernal_api::async_engine::timeout(Duration::from_secs(15), worker).await;
            writer.cancel();
            panic!("orphan sweep retained publication or native store exclusion");
        }
        {
            let _publication = server
                .state
                .artifact_publication
                .blocking_read_for(Duration::from_secs(2));
            seed_staged_generation(server.state.artifact_dir.as_path(), &key, &[b"new"]);
            server.state.artifact_store.insert(
                &key,
                &crate::artifact::ArtifactIndex::new(
                    vec!["new.o".into()],
                    vec![3],
                    vec![],
                    vec![],
                    0,
                ),
            );
        }
        server.state.artifact_store.flush_incremental().unwrap();
        server.state.artifact_store.flush().unwrap();
        let graph_path = super::super::depgraph_file_path_for_cache_dir(&root);
        let graph = server.state.dep_graph.load();
        let context = graph.register(crate::depgraph::CompileContext {
            source_file: root.join("new.c"),
            include_search: Default::default(),
            defines: vec![],
            undefines: vec![],
            flags: vec![],
            force_includes: vec![],
            unknown_flags: vec![],
            compiler_hash: crate::hash::hash_bytes(b"compiler"),
        });
        graph
            .update(
                &context,
                crate::depgraph::ScanResult {
                    resolved: vec![],
                    unresolved: vec![],
                    has_computed: false,
                },
                |_| Some(crate::hash::hash_bytes(b"new")),
            )
            .unwrap();
        graph.persist_incremental(graph_path.as_path()).unwrap();
        crate::depgraph::save_to_file(&graph, graph_path.as_path()).unwrap();
        hook.resume();
        let result = kernal_api::async_engine::timeout(Duration::from_secs(15), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        writer.cancel();
        assert_eq!(result, (0, 1));
        server.state.artifact_store.load_from_disk().unwrap();
        assert_eq!(
            server
                .state
                .artifact_store
                .get(&key)
                .unwrap()
                .output_names
                .as_ref(),
            &["new.o"]
        );
        assert_eq!(verified_sizes(&server.state, &key), vec![3]);
        assert_eq!(
            crate::depgraph::load_from_file(graph_path.as_path())
                .unwrap()
                .contexts_with_artifact_key(),
            1
        );
    }
}
