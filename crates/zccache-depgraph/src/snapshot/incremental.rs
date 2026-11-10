//! Incremental context batches between whole-graph checkpoints.
use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use zccache_artifact::journal::{decode_record, encode_record, Journal, JournalState};
type Row = ([u8; 32], Option<ContextEntrySnapshot>);
fn namespace() -> String {
    format!("depgraph-context-v{DEPGRAPH_VERSION}")
}
fn io_error(error: impl std::fmt::Display) -> SnapshotError {
    SnapshotError::Io(std::io::Error::other(error.to_string()))
}
fn legacy_contexts(
    path: &Path,
    opts: &LoadOptions,
) -> Result<Vec<ContextEntrySnapshot>, SnapshotError> {
    match super::persistence::load_snapshot_with(path, opts) {
        Ok(graph) => Ok(graph.to_snapshot().contexts),
        Err(SnapshotError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(Vec::new())
        }
        Err(error) => Err(error),
    }
}
fn replay(
    view: zccache_artifact::journal::JournalView,
    legacy: Vec<ContextEntrySnapshot>,
) -> Result<BTreeMap<[u8; 32], ContextEntrySnapshot>, SnapshotError> {
    let mut contexts: BTreeMap<_, _> = match view.checkpoint {
        Some(bytes) => decode_record::<Vec<ContextEntrySnapshot>>(&bytes).map_err(io_error)?,
        None => legacy,
    }
    .into_iter()
    .map(|entry| (entry.context_key, entry))
    .collect();
    for bytes in view.records {
        for (key, value) in decode_record::<Vec<Row>>(&bytes).map_err(io_error)? {
            match value {
                Some(value) => {
                    contexts.insert(key, value);
                }
                None => {
                    contexts.remove(&key);
                }
            }
        }
    }
    Ok(contexts)
}
impl DepGraph {
    fn journal(&self, path: &Path) -> Journal<'_> {
        Journal::new(
            &path.with_extension("entries"),
            &namespace(),
            &self.journal_state,
        )
    }
    #[must_use]
    pub fn needs_checkpoint(&self) -> bool {
        self.journal_state.needs_checkpoint()
    }
    pub fn persist_incremental(&self, path: &Path) -> Result<(), SnapshotError> {
        let _permit = self
            .persistence_gate
            .blocking_acquire_for(zccache_core::sync::STATE_TIMEOUT);
        self.persist_incremental_locked(path)
    }
    pub(super) fn persist_incremental_locked(&self, path: &Path) -> Result<(), SnapshotError> {
        if !self.has_pending_contexts() {
            return Ok(());
        }
        let journal = self.journal(path);
        if self.clear_requested.swap(false, Ordering::AcqRel) {
            let result = journal.record_ids().and_then(|ids| {
                journal.checkpoint(&encode_record(&Vec::<ContextEntrySnapshot>::new())?, &ids)
            });
            if let Err(error) = result {
                self.clear_requested.store(true, Ordering::Release);
                return Err(io_error(error));
            }
        }
        if !journal.has_checkpoint().map_err(io_error)? {
            let contexts = legacy_contexts(path, &LoadOptions::default())?;
            journal
                .initialize(&encode_record(&contexts).map_err(io_error)?)
                .map_err(io_error)?;
        }
        let keys: Vec<_> = self
            .pending_contexts
            .iter()
            .map(|entry| *entry.key())
            .collect();
        let pending: Vec<_> = keys
            .into_iter()
            .filter_map(|key| self.pending_contexts.remove(&key))
            .collect();
        let rows: Vec<Row> = pending
            .iter()
            .map(|(key, accessed)| {
                // The queued marker can trail a newer registration/removal.
                // Current state decides presence; the marker only refreshes age.
                let value = self.contexts.get(key).map(|entry| {
                    let mut snapshot = self.snapshot_context(key, &entry);
                    snapshot.last_accessed_unix_ms = snapshot
                        .last_accessed_unix_ms
                        .max(accessed.unwrap_or_default());
                    snapshot
                });
                (*key.hash().as_bytes(), value)
            })
            .collect();
        if !rows.is_empty() {
            if let Err(error) = encode_record(&rows).and_then(|bytes| journal.append(&bytes)) {
                for (key, stamp) in pending {
                    self.pending_contexts.entry(key).or_insert(stamp);
                }
                return Err(io_error(error));
            }
        }
        self.migration_requested.store(false, Ordering::Release);
        Ok(())
    }
    pub(super) fn checkpoint_incremental_locked(&self, path: &Path) -> Result<(), SnapshotError> {
        self.persist_incremental_locked(path)?;
        let journal = self.journal(path);
        let view = journal.read().map_err(io_error)?;
        let covered = view.covered.clone();
        let contexts: Vec<_> = replay(view, Vec::new())?.into_values().collect();
        journal
            .checkpoint(&encode_record(&contexts).map_err(io_error)?, &covered)
            .map_err(io_error)
    }
}
pub(super) fn load_committed(
    path: &Path,
    opts: &LoadOptions,
) -> Result<Option<DepGraph>, SnapshotError> {
    if !path.with_extension("entries").exists() {
        return Ok(None);
    }
    let state = JournalState::default();
    let journal = Journal::new(&path.with_extension("entries"), &namespace(), &state);
    let view = journal.read().map_err(io_error)?;
    let migrated = view.checkpoint.is_some();
    if !migrated && view.records.is_empty() {
        return Ok(None);
    }
    let legacy = if migrated {
        Vec::new()
    } else {
        legacy_contexts(path, opts)?
    };
    let contexts = replay(view, legacy)?;
    let ttl_ms = u64::try_from(opts.ttl.as_millis()).unwrap_or(u64::MAX);
    let (contexts, expired): (Vec<_>, Vec<_>) = contexts.into_values().partition(|context| {
        opts.now_unix_ms
            .saturating_sub(context.last_accessed_unix_ms)
            <= ttl_ms
    });
    let snapshot = DepGraphSnapshot {
        stats: SnapshotStats {
            saved_at_epoch_secs: opts.now_unix_ms / 1000,
            file_count: 0,
            context_count: contexts.len() as u64,
        },
        // Include-scan nodes are derived data. Rebuild them rather than retain
        // stale nodes from a checkpoint older than these committed contexts.
        files: Vec::new(),
        contexts,
    };
    let mut graph = DepGraph::from_snapshot(snapshot);
    graph.journal_state = state;
    if !expired.is_empty() {
        for context in expired {
            graph.queue_removed_context(ContextKey::from_raw(context.context_key));
        }
        graph.mark_dirty();
    }
    if !migrated {
        graph.schedule_incremental_migration();
    }
    Ok(Some(graph))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::ScanResult;

    #[test]
    fn incremental_delayed_eviction_does_not_remove_a_reregistered_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("depgraph.bin");
        let graph = DepGraph::new();
        let key = graph.register(super::super::tests::make_ctx("reregistered.c"));
        graph
            .update(
                &key,
                ScanResult {
                    resolved: Vec::new(),
                    unresolved: Vec::new(),
                    has_computed: false,
                },
                super::super::tests::dummy_hash,
            )
            .unwrap();
        // An older eviction queues its removal after re-registration completed.
        let map_key = *graph.contexts_iter().next().unwrap().key();
        graph.queue_removed_context(map_key);
        graph.persist_incremental(&path).unwrap();
        assert_eq!(
            load_from_file(&path).unwrap().contexts_with_artifact_key(),
            1
        );
    }

    #[test]
    fn incremental_late_env_dep_update_is_committed_with_the_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("depgraph.bin");
        let source = dir.path().join("env.c");
        std::fs::write(&source, b"int env;").unwrap();
        let graph = DepGraph::new();
        let key = graph.register(super::super::tests::make_ctx(&source.to_string_lossy()));
        graph
            .update(
                &key,
                ScanResult {
                    resolved: Vec::new(),
                    unresolved: Vec::new(),
                    has_computed: false,
                },
                |path| zccache_hash::hash_file(path).ok(),
            )
            .unwrap();
        let value = zccache_hash::hash_bytes(b"new value");
        graph.set_rustc_env_deps(key, vec![("MODE".into(), Some(value))]);
        graph.persist_incremental(&path).unwrap();
        let deps = load_from_file(&path)
            .unwrap()
            .get_rustc_env_deps(&key)
            .unwrap();
        assert_eq!(
            deps.len(),
            1,
            "late env-dep updates must not be lost behind an older queued snapshot"
        );
        assert_eq!(deps[0], ("MODE".to_string(), Some(value)));
    }

    #[test]
    fn incremental_startup_schedules_legacy_context_migration_without_a_compile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("depgraph.bin");
        let graph = DepGraph::new();
        graph.register(super::super::tests::make_ctx("legacy.c"));
        save_to_file(&graph, &path).unwrap();
        let loaded = load_from_file(&path).unwrap();
        assert!(
            loaded.has_pending_contexts(),
            "legacy startup must schedule migration without a new compile"
        );
        loaded.persist_incremental(&path).unwrap();
        assert!(!loaded.has_pending_contexts());
        std::fs::write(&path, b"obsolete checkpoint").unwrap();
        assert_eq!(load_from_file(&path).unwrap().stats().context_count, 1);
    }

    #[test]
    fn incremental_context_access_survives_restart_without_expiring_an_active_hit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("depgraph.bin");
        let source = dir.path().join("active.c");
        std::fs::write(&source, b"int active;").unwrap();
        let graph = DepGraph::new();
        let key = graph.register(super::super::tests::make_ctx(&source.to_string_lossy()));
        graph
            .update(
                &key,
                ScanResult {
                    resolved: Vec::new(),
                    unresolved: Vec::new(),
                    has_computed: false,
                },
                |path| zccache_hash::hash_file(path).ok(),
            )
            .unwrap();
        for mut entry in graph.contexts.iter_mut() {
            entry.last_accessed_unix_ms = 1;
            graph.queue_context(*entry.key(), &entry);
        }
        graph.persist_incremental(&path).unwrap();
        assert!(matches!(
            graph.check(&key, |_| true, |path| zccache_hash::hash_file(path).ok()),
            crate::graph::CacheVerdict::Hit { .. }
        ));
        graph.persist_incremental(&path).unwrap();
        let restored = load_from_file_with(
            &path,
            &LoadOptions {
                ttl: std::time::Duration::from_secs(60),
                now_unix_ms: now_unix_ms(),
            },
        )
        .unwrap();
        assert_eq!(
            restored.contexts_with_artifact_key(),
            1,
            "a recent hit must refresh the committed retention age"
        );
    }

    #[test]
    fn incremental_context_survives_without_a_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("depgraph.bin");
        let graph = DepGraph::new();
        let key = graph.register(super::super::tests::make_ctx("recent.c"));
        graph
            .update(
                &key,
                ScanResult {
                    resolved: Vec::new(),
                    unresolved: Vec::new(),
                    has_computed: false,
                },
                super::super::tests::dummy_hash,
            )
            .unwrap();
        graph.persist_incremental(&path).unwrap();
        assert!(
            !path.exists(),
            "context commits must not rewrite depgraph.bin"
        );
        let restored = load_from_file(&path).unwrap();
        assert_eq!(restored.contexts_with_artifact_key(), 1);
    }

    #[test]
    fn incremental_partial_migration_replays_committed_rows_before_its_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("depgraph.bin");
        let graph = DepGraph::new();
        let key = graph.register(super::super::tests::make_ctx("recent.c"));
        graph
            .update(
                &key,
                ScanResult {
                    resolved: Vec::new(),
                    unresolved: Vec::new(),
                    has_computed: false,
                },
                super::super::tests::dummy_hash,
            )
            .unwrap();
        let started = Instant::now();
        graph.persist_incremental(&path).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        // A batch published before legacy migration completes must also replay.
        let state = JournalState::default();
        let partial = dir.path().join("partial.bin");
        let journal = Journal::new(&partial.with_extension("entries"), &namespace(), &state);
        let rows: Vec<Row> = graph
            .to_snapshot()
            .contexts
            .into_iter()
            .map(|entry| (entry.context_key, Some(entry)))
            .collect();
        journal.append(&encode_record(&rows).unwrap()).unwrap();
        assert_eq!(
            load_from_file(&partial)
                .unwrap()
                .contexts_with_artifact_key(),
            1
        );
        assert_eq!(
            load_from_file(&path).unwrap().contexts_with_artifact_key(),
            1
        );
        graph.clear();
        graph.persist_incremental(&path).unwrap();
        assert_eq!(
            load_from_file(&path).unwrap().contexts_with_artifact_key(),
            0
        );
    }
}
