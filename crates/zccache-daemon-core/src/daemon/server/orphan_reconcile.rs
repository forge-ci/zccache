//! Startup repair of payloads left behind before an index commit.

use super::{IndexWriterCommand, SharedState};
use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};
use zccache_core::NormalizedPath;

fn known(state: &SharedState, key: &str) -> bool {
    state.artifacts.contains_key(key) || state.artifact_store.get(key).is_some()
}

fn legacy_orphans(
    state: &SharedState,
    started: Instant,
    budget: Duration,
) -> io::Result<Vec<(String, NormalizedPath)>> {
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
            orphans.push((key.to_string(), NormalizedPath::new(entry.path())));
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

/// Publication exclusion plus a writer acknowledgment prevents deleting a
/// newly published payload whose row is still waiting in the index queue.
pub(super) fn reconcile(state: &SharedState, budget: Duration) -> io::Result<(usize, usize)> {
    if budget.is_zero() {
        return Ok((0, 0));
    }
    let started = Instant::now();
    let _publication = state.artifact_publication.blocking_write_for(budget);
    let mut staged: HashMap<_, _> =
        super::persist::scan_v2_disk_artifacts(state.artifact_dir.as_path())?
            .into_iter()
            .filter(|entry| !known(state, &entry.key))
            .map(|entry| (entry.key, entry.generation))
            .collect();
    let legacy = legacy_orphans(state, started, budget)?;
    if staged.is_empty() && legacy.is_empty() {
        return Ok((0, 0));
    }
    flush_queued_index(state, budget.saturating_sub(started.elapsed()))?;
    let outcome = crate::artifact::reconcile_missing_index_entries(
        state.artifact_dir.as_path(),
        budget.saturating_sub(started.elapsed()),
        |key| known(state, key),
    );
    let mut recovered = 0;
    for (key, meta) in outcome.entries {
        if !known(state, &key) {
            state.artifact_store.insert(&key, &meta);
            recovered += 1;
        }
    }
    let deletions: HashMap<_, _> = outcome
        .unrecoverable_keys
        .into_iter()
        .filter(|key| !known(state, key))
        .filter_map(|key| staged.remove(&key).map(|generation| (key, generation)))
        .collect();
    let mut removed = super::persist::evict_v2_artifact_keys_if_unchanged(
        state.artifact_dir.as_path(),
        &deletions,
    )?;
    for (key, path) in legacy {
        if started.elapsed() >= budget {
            break;
        }
        if !known(state, &key) {
            match super::remove_cow_blob(&path) {
                Ok(()) => {
                    removed.insert(key);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    state.dep_graph.load().invalidate_artifact_keys(&removed);
    state.artifact_store.flush_incremental()?;
    if recovered > 0 || !removed.is_empty() {
        crate::core::lifecycle::write_event_in_cache_root(
            state.cache_dir.as_path(),
            crate::core::lifecycle::EVENT_INDEX_RECONCILED,
            serde_json::json!({ "reason": "startup_orphans", "recovered": recovered, "removed": removed.len(), "truncated_by_budget": outcome.truncated_by_budget }),
        );
    }
    Ok((recovered, removed.len()))
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
}
