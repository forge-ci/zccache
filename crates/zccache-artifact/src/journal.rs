//! Checksummed per-writer batches over the existing atomic KV owner.
//! Checkpoint/GC callers quiesce publishers under the cache-root ownership lease.
//! Consumed IDs make interrupted GC harmless; readers retry concurrent checkpoints.

use crate::{Key, KvError, KvStore, MAX_VALUE_BYTES};
use bincode::Options;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_PAYLOAD: u64 = 1024 * 1024 * 1024;
const CHUNK_BYTES: usize = MAX_VALUE_BYTES / 2;
const MAX_BATCHES: usize = 2048;
static CLOCK: AtomicU64 = AtomicU64::new(0);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Size-limited encoding for the versioned journal's metadata records.
pub fn encode_record<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PAYLOAD)
        .serialize(value)
        .map_err(invalid)
}

/// Strict decoding bounded by the bytes actually stored.
pub fn decode_record<T: DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(invalid)
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
fn kv_error(error: KvError) -> io::Error {
    match error {
        KvError::Io(error) => error,
        error => invalid(error),
    }
}

/// Scheduling state; construction performs no filesystem I/O.
#[derive(Default)]
pub struct JournalState {
    batches: AtomicUsize,
    observed_ns: AtomicU64,
}

impl JournalState {
    /// The daemon checkpoints after a bounded number of append batches.
    #[must_use]
    pub fn needs_checkpoint(&self) -> bool {
        self.batches.load(Ordering::Acquire) >= MAX_BATCHES
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Stamp {
    tick_ns: u64,
    pid: u32,
    sequence: u64,
}
#[derive(Serialize, Deserialize)]
enum Payload {
    Inline(Vec<u8>),
    Chunks { size: u64, keys: Vec<[u8; 32]> },
}
#[derive(Serialize, Deserialize)]
struct Segment {
    stamp: Stamp,
    payload: Payload,
}
#[derive(Serialize, Deserialize)]
struct Checkpoint {
    revision: Stamp,
    consumed: Vec<[u8; 32]>,
    payload: Payload,
}

/// A consistent checkpoint and its not-yet-folded append batches.
pub struct JournalView {
    pub checkpoint: Option<Vec<u8>>,
    pub records: Vec<Vec<u8>>,
    pub covered: Vec<Key>,
}

/// Filesystem methods belong on a blocking lane; construction only owns paths.
pub struct Journal<'a> {
    root: PathBuf,
    records: String,
    checkpoints: String,
    chunks: String,
    state: &'a JournalState,
}

impl<'a> Journal<'a> {
    #[must_use]
    pub fn new(root: &Path, namespace: &str, state: &'a JournalState) -> Self {
        Self {
            root: root.to_path_buf(),
            records: namespace.into(),
            checkpoints: format!("{namespace}-base"),
            chunks: format!("{namespace}-chunks"),
            state,
        }
    }

    fn stamp(&self) -> Stamp {
        let now_ns = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        )
        .unwrap_or(u64::MAX);
        let floor = now_ns.max(
            self.state
                .observed_ns
                .load(Ordering::Acquire)
                .saturating_add(1),
        );
        let previous = CLOCK
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |old| {
                Some(floor.max(old.saturating_add(1)))
            })
            .unwrap_or_else(|old| old);
        let tick_ns = floor.max(previous.saturating_add(1));
        self.state.observed_ns.fetch_max(tick_ns, Ordering::AcqRel);
        Stamp {
            tick_ns,
            pid: std::process::id(),
            sequence: SEQUENCE.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn kv(&self) -> io::Result<KvStore> {
        KvStore::open(&self.root).map_err(kv_error)
    }

    fn store_payload(
        &self,
        kv: &KvStore,
        bytes: &[u8],
        inline_limit: usize,
    ) -> io::Result<Payload> {
        if bytes.len() as u64 > MAX_PAYLOAD {
            return Err(invalid("journal payload exceeds its size budget"));
        }
        if bytes.len() <= inline_limit {
            return Ok(Payload::Inline(bytes.to_vec()));
        }
        let mut keys = Vec::new();
        for chunk in bytes.chunks(CHUNK_BYTES) {
            let key = Key::from_hash(kernal_api::hash::blake3_bytes(chunk));
            kv.put(&self.chunks, &key, chunk).map_err(kv_error)?;
            keys.push(key.0);
        }
        Ok(Payload::Chunks {
            size: bytes.len() as u64,
            keys,
        })
    }

    fn load_payload(&self, kv: &KvStore, payload: &Payload) -> io::Result<Vec<u8>> {
        match payload {
            Payload::Inline(bytes) => Ok(bytes.clone()),
            Payload::Chunks { size, keys } => {
                if *size > MAX_PAYLOAD || keys.len() as u64 != size.div_ceil(CHUNK_BYTES as u64) {
                    return Err(invalid("invalid journal chunk count or size"));
                }
                let mut bytes = Vec::with_capacity(*size as usize);
                for key in keys {
                    let chunk = kv
                        .get(&self.chunks, &Key(*key))
                        .map_err(kv_error)?
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::NotFound, "journal chunk missing")
                        })?;
                    if chunk.len() != (*size as usize - bytes.len()).min(CHUNK_BYTES) {
                        return Err(invalid("journal chunk size mismatch"));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(bytes)
            }
        }
    }

    fn checkpoint_record(&self, kv: &KvStore) -> io::Result<Option<Checkpoint>> {
        kv.get(&self.checkpoints, &Key([0; 32]))
            .map_err(kv_error)?
            .map(|bytes| decode_record(&bytes))
            .transpose()
    }

    pub fn has_checkpoint(&self) -> io::Result<bool> {
        Ok(self.checkpoint_record(&self.kv()?)?.is_some())
    }

    /// Ordinary batches require one atomic publication/fsync, not one per row.
    pub fn append(&self, bytes: &[u8]) -> io::Result<()> {
        let kv = self.kv()?;
        let payload = self.store_payload(&kv, bytes, CHUNK_BYTES)?;
        let stamp = self.stamp();
        let key = Key::from_hash(kernal_api::hash::blake3_bytes(&encode_record(&stamp)?));
        kv.put(
            &self.records,
            &key,
            &encode_record(&Segment { stamp, payload })?,
        )
        .map_err(kv_error)?;
        self.state.batches.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn read_contents(
        &self,
        kv: &KvStore,
        checkpoint: Option<&Checkpoint>,
        covered: &[Key],
    ) -> io::Result<JournalView> {
        let consumed: HashSet<_> = checkpoint
            .into_iter()
            .flat_map(|value| value.consumed.iter().copied())
            .collect();
        let base = checkpoint
            .map(|value| self.load_payload(kv, &value.payload))
            .transpose()?;
        let mut records: Vec<Segment> = Vec::new();
        for key in covered.iter().filter(|key| !consumed.contains(&key.0)) {
            let bytes = kv
                .get(&self.records, key)
                .map_err(kv_error)?
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "journal batch moved during read")
                })?;
            records.push(decode_record(&bytes)?);
        }
        records.sort_by_key(|record| record.stamp);
        let mut payloads = Vec::with_capacity(records.len());
        for record in records {
            self.state
                .observed_ns
                .fetch_max(record.stamp.tick_ns, Ordering::AcqRel);
            payloads.push(self.load_payload(kv, &record.payload)?);
        }
        Ok(JournalView {
            checkpoint: base,
            records: payloads,
            covered: covered.to_vec(),
        })
    }

    pub fn read(&self) -> io::Result<JournalView> {
        if !self.root.exists() {
            return Ok(JournalView {
                checkpoint: None,
                records: Vec::new(),
                covered: Vec::new(),
            });
        }
        let kv = self.kv()?;
        for _ in 0..3 {
            let before = self.checkpoint_record(&kv)?;
            let covered = kv.namespace_keys(&self.records).map_err(kv_error)?;
            let contents = self.read_contents(&kv, before.as_ref(), &covered);
            let after = self.checkpoint_record(&kv)?;
            if before.as_ref().map(|value| value.revision)
                == after.as_ref().map(|value| value.revision)
            {
                let view = contents?;
                if let Some(checkpoint) = before {
                    self.state
                        .observed_ns
                        .fetch_max(checkpoint.revision.tick_ns, Ordering::AcqRel);
                }
                self.state
                    .batches
                    .store(view.records.len(), Ordering::Release);
                return Ok(view);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "journal checkpoint kept changing during read",
        ))
    }

    /// Initializers never reclaim chunks another initializer may be publishing.
    pub fn initialize(&self, bytes: &[u8]) -> io::Result<()> {
        if !self.has_checkpoint()? {
            self.write_checkpoint(bytes, &[], false)?;
        }
        Ok(())
    }

    /// Requires quiesced publishers and the cache-root writer lease.
    pub fn checkpoint(&self, bytes: &[u8], covered: &[Key]) -> io::Result<()> {
        self.write_checkpoint(bytes, covered, true)
    }

    fn write_checkpoint(&self, bytes: &[u8], covered: &[Key], reclaim: bool) -> io::Result<()> {
        let kv = self.kv()?;
        let payload = self.store_payload(&kv, bytes, 64 * 1024)?;
        let retained: HashSet<_> = match &payload {
            Payload::Inline(_) => HashSet::new(),
            Payload::Chunks { keys, .. } => keys.iter().copied().collect(),
        };
        let checkpoint = Checkpoint {
            revision: self.stamp(),
            consumed: covered.iter().map(|key| key.0).collect(),
            payload,
        };
        kv.put(
            &self.checkpoints,
            &Key([0; 32]),
            &encode_record(&checkpoint)?,
        )
        .map_err(kv_error)?;
        if reclaim {
            for key in covered {
                if let Err(error) = kv.remove(&self.records, key) {
                    tracing::warn!(%error, "journal batch reclamation deferred");
                }
            }
            for key in kv.namespace_keys(&self.chunks).map_err(kv_error)? {
                if !retained.contains(&key.0) {
                    if let Err(error) = kv.remove(&self.chunks, &key) {
                        tracing::warn!(%error, "journal chunk reclamation deferred");
                    }
                }
            }
            self.state.batches.store(0, Ordering::Release);
        }
        Ok(())
    }

    /// A clear captures IDs without reading old batch payloads.
    pub fn record_ids(&self) -> io::Result<Vec<Key>> {
        self.kv()?.namespace_keys(&self.records).map_err(kv_error)
    }

    pub fn stored_bytes(&self) -> io::Result<u64> {
        let kv = self.kv()?;
        [&self.records, &self.checkpoints, &self.chunks]
            .into_iter()
            .try_fold(0_u64, |total, namespace| {
                kv.namespace_bytes(namespace)
                    .map_err(kv_error)
                    .map(|bytes| total.saturating_add(bytes))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incremental_journal_corrupt_headers_are_errors_instead_of_missing_commits() {
        for corrupt_base in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let state = JournalState::default();
            let journal = Journal::new(temp.path(), "journal-test", &state);
            journal.initialize(b"base").unwrap();
            journal.append(b"committed batch").unwrap();
            let (namespace, key) = if corrupt_base {
                (&journal.checkpoints, Key([0; 32]))
            } else {
                (&journal.records, journal.record_ids().unwrap()[0])
            };
            let path = temp
                .path()
                .join("kv")
                .join(namespace)
                .join(format!("{}.bin", key.to_hex()));
            std::fs::write(path, b"broken header").unwrap();
            assert!(
                journal.read().is_err(),
                "committed files cannot silently disappear"
            );
            if corrupt_base {
                assert!(journal.initialize(b"stale legacy").is_err());
            }
        }
    }

    #[test]
    fn incremental_journal_replay_and_compaction_do_not_resurrect_consumed_batches() {
        let temp = tempfile::tempdir().unwrap();
        let state = JournalState::default();
        let journal = Journal::new(temp.path(), "journal-test", &state);
        journal.initialize(b"initial").unwrap();
        journal.append(b"old upsert").unwrap();
        let view = journal.read().unwrap();
        let kv = journal.kv().unwrap();
        let old = kv.get(&journal.records, &view.covered[0]).unwrap().unwrap();
        journal.checkpoint(b"removed", &view.covered).unwrap();
        // Simulate power loss after publication but before old-batch GC.
        kv.put(&journal.records, &view.covered[0], &old).unwrap();
        let restored = journal.read().unwrap();
        assert_eq!(restored.checkpoint.as_deref(), Some(b"removed".as_slice()));
        assert!(restored.records.is_empty());
        journal.append(b"new upsert").unwrap();
        assert_eq!(
            journal.read().unwrap().records,
            vec![b"new upsert".to_vec()]
        );
    }
    #[test]
    fn incremental_journal_chunked_checkpoint_reopens_and_reclaims_old_chunks() {
        let temp = tempfile::tempdir().unwrap();
        let state = JournalState::default();
        let journal = Journal::new(temp.path(), "journal-test", &state);
        let bytes = vec![17; 128 * 1024];
        journal.initialize(&bytes).unwrap();
        assert_eq!(journal.read().unwrap().checkpoint.unwrap(), bytes);
        journal.checkpoint(b"small replacement", &[]).unwrap();
        assert!(journal
            .kv()
            .unwrap()
            .list_namespace(&journal.chunks)
            .unwrap()
            .is_empty());
        assert_eq!(
            journal.read().unwrap().checkpoint.unwrap(),
            b"small replacement"
        );
    }
}
