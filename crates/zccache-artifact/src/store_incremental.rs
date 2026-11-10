//! Batched durable metadata over the existing checksummed KV owner.
use super::{ArtifactIndex, ArtifactStore};
use crate::journal::{decode_record, encode_record, Journal};
use std::collections::BTreeMap;
use std::sync::{atomic::Ordering, Arc};
const NAMESPACE: &str = "artifact-index-v1";
type Row = (String, Option<ArtifactIndex>);
fn io_error(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(error.to_string())
}
impl ArtifactStore {
    /// Whether either supported persistence format is present at this path.
    #[must_use]
    pub fn has_persisted_index(path: &std::path::Path) -> bool {
        path.exists() || path.with_extension("entries").exists()
    }

    /// Strict disk-export loading: never downgrade a corrupt index to an empty one.
    pub(crate) fn open_export_source(path: &std::path::Path) -> std::io::Result<Self> {
        let store = Self::open_empty(path);
        if store.load_committed_entries()? {
            return Ok(store);
        }
        let rows = match std::fs::read(path) {
            Ok(bytes) => super::decode_index_rows(&bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && Self::has_persisted_index(path) =>
            {
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        for (key, value) in store.overlay_incremental(rows)? {
            store.entries.insert(key, value);
        }
        Ok(store)
    }

    #[must_use]
    pub fn has_pending_entries(&self) -> bool {
        !self.pending.is_empty()
            || self.migration_requested.load(Ordering::Acquire)
            || self.clear_requested.load(Ordering::Acquire)
    }
    #[must_use]
    pub fn needs_checkpoint(&self) -> bool {
        self.journal_state.needs_checkpoint()
    }
    fn journal(&self) -> Journal<'_> {
        Journal::new(
            &self.path.as_path().with_extension("entries"),
            NAMESPACE,
            &self.journal_state,
        )
    }
    pub fn flush_incremental(&self) -> std::io::Result<()> {
        let _guard = self
            .flush_lock
            .blocking_acquire_for(zccache_core::sync::STATE_TIMEOUT);
        self.flush_incremental_locked()
    }
    fn legacy_rows(&self) -> std::io::Result<Vec<(String, ArtifactIndex)>> {
        match std::fs::read(self.path.as_path()) {
            Ok(bytes) => match super::decode_index_rows(&bytes) {
                Ok(rows) => Ok(rows),
                Err(_) if self.started_corrupt.load(Ordering::Acquire) => Ok(Vec::new()),
                Err(error) => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }
    pub(super) fn flush_incremental_locked(&self) -> std::io::Result<()> {
        if !self.has_pending_entries() {
            return Ok(());
        }
        let journal = self.journal();
        if self.clear_requested.swap(false, Ordering::AcqRel) {
            let result = journal.record_ids().and_then(|ids| {
                journal.checkpoint(
                    &encode_record(&Vec::<(String, ArtifactIndex)>::new())?,
                    &ids,
                )
            });
            if let Err(error) = result {
                self.clear_requested.store(true, Ordering::Release);
                return Err(error);
            }
        }
        if !journal.has_checkpoint()? {
            journal.initialize(&encode_record(&self.legacy_rows()?)?)?;
        }
        let keys: Vec<_> = self
            .pending
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        let rows: Vec<Row> = keys
            .into_iter()
            .filter_map(|key| self.pending.remove(&key))
            .collect();
        if !rows.is_empty() {
            if let Err(error) = encode_record(&rows).and_then(|bytes| journal.append(&bytes)) {
                for (key, value) in rows {
                    self.pending.entry(key).or_insert(value);
                }
                return Err(error);
            }
        }
        self.migration_requested.store(false, Ordering::Release);
        Ok(())
    }
    pub(super) fn checkpoint_incremental_locked(&self) -> std::io::Result<()> {
        self.flush_incremental_locked()?;
        let journal = self.journal();
        let view = journal.read()?;
        let rows = self.overlay_incremental(Vec::new())?;
        journal.checkpoint(&encode_record(&rows)?, &view.covered)?;
        self.write_legacy_rows(&rows)
    }
    pub(super) fn load_committed_entries(&self) -> std::io::Result<bool> {
        if !self.path.as_path().with_extension("entries").exists()
            || !self.journal().has_checkpoint()?
        {
            return Ok(false);
        }
        for (key, value) in self.overlay_incremental(Vec::new())? {
            self.entries.entry(key).or_insert(value);
        }
        Ok(true)
    }
    pub async fn flush_incremental_async(self: Arc<Self>) -> std::io::Result<()> {
        kernal_api::async_engine::launch_blocking(move || self.flush_incremental())
            .await
            .map_err(io_error)?
    }
    pub(super) fn overlay_incremental(
        &self,
        rows: Vec<(String, ArtifactIndex)>,
    ) -> std::io::Result<Vec<(String, ArtifactIndex)>> {
        let view = self.journal().read()?;
        let mut rows: BTreeMap<String, ArtifactIndex> = match view.checkpoint {
            Some(bytes) => decode_record::<Vec<(String, ArtifactIndex)>>(&bytes)?,
            None => rows,
        }
        .into_iter()
        .collect();
        for bytes in view.records {
            for (key, value) in decode_record::<Vec<Row>>(&bytes)? {
                match value {
                    Some(value) => {
                        rows.insert(key, value);
                    }
                    None => {
                        rows.remove(&key);
                    }
                }
            }
        }
        Ok(rows.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ArtifactIndex;

    fn meta() -> ArtifactIndex {
        ArtifactIndex::new(vec!["result.o".into()], vec![4], vec![], vec![], 0)
    }

    #[test]
    fn incremental_startup_deferred_load_preserves_legacy_rows() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index.bin");
        let legacy = ArtifactStore::open_empty(&path);
        legacy.insert("legacy", &meta());
        legacy.flush().unwrap();
        let deferred = ArtifactStore::open_empty(&path);
        deferred.insert("recent", &meta());
        deferred.flush_incremental().unwrap();
        let restored = ArtifactStore::open(&path).unwrap();
        assert!(
            restored.get("legacy").is_some(),
            "early writes must import the complete deferred legacy index"
        );
        assert!(restored.get("recent").is_some());
    }

    #[test]
    fn incremental_startup_clear_retires_legacy_metadata_without_per_key_writes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index.bin");
        let store = ArtifactStore::open_empty(&path);
        for index in 0..10 {
            store.insert(&format!("old-{index}"), &meta());
        }
        store.flush().unwrap();
        store.clear();
        store.flush_incremental().unwrap();
        assert!(ArtifactStore::open(&path).unwrap().is_empty());
        assert!(
            store.journal().record_ids().unwrap().is_empty(),
            "a global clear must retire the namespace, not retain one tombstone file per old key"
        );
    }

    #[test]
    fn incremental_writer_child() {
        let Ok(encoded) = std::env::var("ZCCACHE_TEST_ENTRY_WRITER") else {
            return;
        };
        let (path, prefix): (std::path::PathBuf, String) = serde_json::from_str(&encoded).unwrap();
        let store = ArtifactStore::open_empty(&path);
        for index in 0..100 {
            store.insert(&format!("{prefix}-{index}"), &meta());
        }
        store.flush_incremental().unwrap();
    }

    #[test]
    fn incremental_two_processes_keep_every_committed_entry() {
        use zccache_test_support::test_child::TestChild;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index.bin");
        let children: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|prefix| {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args([
                        "--exact",
                        "store::incremental::tests::incremental_writer_child",
                    ])
                    .env(
                        "ZCCACHE_TEST_ENTRY_WRITER",
                        serde_json::to_string(&(&path, prefix)).unwrap(),
                    );
                TestChild::spawn(command).unwrap()
            })
            .collect();
        for child in children {
            let output = child.finish(std::time::Duration::from_secs(60)).unwrap();
            assert_eq!(output.code, Some(0), "{}", output.stderr_text());
        }
        let restored = ArtifactStore::open(&path).unwrap();
        assert_eq!(restored.len(), 200);
        for prefix in ["first", "second"] {
            for index in 0..100 {
                assert!(restored.get(&format!("{prefix}-{index}")).is_some());
            }
        }
    }

    #[test]
    fn incremental_startup_migrates_legacy_rows_once_and_ignores_the_old_blob() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index.bin");
        let legacy = ArtifactStore::open_empty(&path);
        legacy.insert("legacy", &meta());
        legacy.flush().unwrap();
        let migrated = ArtifactStore::open(&path).unwrap();
        assert!(
            migrated.has_pending_entries(),
            "legacy startup must schedule migration without a new compile"
        );
        migrated.insert("new", &meta());
        migrated.flush_incremental().unwrap();
        std::fs::write(&path, b"obsolete checkpoint").unwrap();
        let reopened = ArtifactStore::open(&path).unwrap();
        assert!(reopened.get("legacy").is_some());
        assert!(reopened.get("new").is_some());
    }

    #[test]
    fn incremental_failed_commit_keeps_the_entry_pending_for_retry() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index.bin");
        std::fs::write(path.with_extension("entries"), b"not a directory").unwrap();
        let store = ArtifactStore::open_empty(&path);
        store.insert("retry", &meta());
        assert!(store.flush_incremental().is_err());
        assert!(store.has_pending_entries());
        std::fs::remove_file(path.with_extension("entries")).unwrap();
        store.flush_incremental().unwrap();
        assert!(ArtifactStore::open(&path).unwrap().get("retry").is_some());
    }

    #[test]
    fn incremental_persist_budget_bytes_do_not_grow_between_1k_and_10k_entries() {
        let mut deltas = Vec::new();
        for count in [1_000, 10_000] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("index.bin");
            let store = ArtifactStore::open_empty(&path);
            for index in 0..count {
                store.insert(&format!("seed-{index}"), &meta());
            }
            store.flush_incremental().unwrap();
            let bytes = || store.journal().stored_bytes().unwrap();
            let before = bytes();
            let started = std::time::Instant::now();
            store.insert("added", &meta());
            store.flush_incremental().unwrap();
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
            deltas.push(bytes() - before);
        }
        assert!(deltas[0] > 0);
        assert_eq!(
            deltas[0], deltas[1],
            "one commit writes one row at either store size"
        );
    }

    #[test]
    fn incremental_tombstone_does_not_resurrect_a_checkpoint_entry() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index.bin");
        let store = ArtifactStore::open_empty(&path);
        store.insert("removed", &meta());
        store.flush().unwrap();
        assert!(store.remove("removed"));
        store.flush_incremental().unwrap();
        assert!(ArtifactStore::open(&path).unwrap().get("removed").is_none());
    }

    #[test]
    fn incremental_lookup_stays_in_memory_under_a_duration_budget() {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open_empty(&temp.path().join("index.bin"));
        store.insert("hit", &meta());
        store.flush_incremental().unwrap();
        let started = std::time::Instant::now();
        for _ in 0..10_000 {
            std::hint::black_box(store.get("hit").unwrap());
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn incremental_commit_survives_reopen_without_a_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index.bin");
        let store = ArtifactStore::open_empty(&path);
        let meta = ArtifactIndex::new(vec!["result.o".into()], vec![4], vec![], vec![], 0);
        store.insert("recent", &meta);
        store.flush_incremental().unwrap();
        // No checkpoint or shutdown drain: only the per-entry commit may exist.
        assert!(
            !path.exists(),
            "incremental commits must not rewrite index.bin"
        );
        let reopened = ArtifactStore::open(&path).unwrap();
        assert_eq!(
            reopened.get("recent").unwrap().output_names,
            meta.output_names
        );
    }
}
