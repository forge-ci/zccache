//! File metadata types and cache implementation.

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};
use zccache_core::NormalizedPath;

// ── mtime drift tolerance (issue #469) ──────────────────────────────────────

/// Default tolerated mtime drift, in nanoseconds, when comparing a cached
/// metadata entry against a fresh `stat()` (`get_cached_hash_if_stat_valid`).
///
/// Set to 1 full second so archive-extraction truncation (tar / zstd-tar /
/// soldr-save-load all flatten sub-second nanos to 0) is absorbed. The
/// tolerance applies only when either timestamp has a zero sub-second field,
/// which is the signature of that truncation. Native fine-grained timestamps
/// remain exact so a same-size edit cannot be mistaken for an archive restore.
///
/// Override at daemon startup via `ZCCACHE_MTIME_TOLERANCE_NS`:
/// - `0` → strict byte-equal mtime match (pre-issue-#469 behavior).
/// - any positive integer → tolerance window in nanoseconds.
pub const DEFAULT_MTIME_TOLERANCE_NS: u64 = 1_000_000_000;

/// Env var that overrides [`DEFAULT_MTIME_TOLERANCE_NS`]. Read once on first
/// access; subsequent changes during daemon lifetime are ignored.
pub const MTIME_TOLERANCE_ENV: &str = "ZCCACHE_MTIME_TOLERANCE_NS";

/// Resolve the active tolerance, caching the result. The env probe is done
/// exactly once per daemon process — no per-hit syscall overhead.
fn mtime_tolerance_ns() -> u64 {
    static CACHED: OnceLock<u64> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var(MTIME_TOLERANCE_ENV)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MTIME_TOLERANCE_NS)
    })
}

/// Compare two mtimes with the configured drift tolerance.
///
/// Match if:
/// - The two values are byte-equal (no truncation happened), OR
/// - `|a - b| < mtime_tolerance_ns()` and either timestamp has a zero
///   sub-second field, the archive-truncation signature.
///
/// Setting the tolerance to `0` (via `ZCCACHE_MTIME_TOLERANCE_NS=0`)
/// disables the second branch entirely and restores strict byte-equal
/// semantics — useful for diagnosing watcher bugs.
fn mtimes_match(a: SystemTime, b: SystemTime) -> bool {
    if a == b {
        return true;
    }
    let tolerance_ns = mtime_tolerance_ns();
    if tolerance_ns == 0 {
        return false;
    }
    let (a_secs, a_nanos) = mtime_components(a);
    let (b_secs, b_nanos) = mtime_components(b);
    if a_nanos != 0 && b_nanos != 0 {
        return false;
    }
    let a_total = (a_secs as u128) * 1_000_000_000 + (a_nanos as u128);
    let b_total = (b_secs as u128) * 1_000_000_000 + (b_nanos as u128);
    a_total.abs_diff(b_total) < tolerance_ns as u128
}

fn mtime_components(t: SystemTime) -> (u64, u32) {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| (d.as_secs(), d.subsec_nanos()))
        .unwrap_or((0, 0))
}

/// Confidence level for a cached metadata entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// Entry has not been verified recently. Must stat before trusting.
    Low,
    /// Watcher indicates no changes since last verification.
    Medium,
    /// Recently stat-verified or content-hash verified.
    High,
}

/// Cached metadata for a single file.
///
/// Change detection uses `(mtime, size)` plus [`crate::FileIdentity`]
/// (Unix `ctime` + inode, #1943), all from one `stat`. `ctime` catches an
/// in-place edit whose mtime was restored; the inode catches replacement.
/// Where neither exists (Windows) the file watcher (`notify`) is the backstop.
#[derive(Debug, Clone)]
pub struct FileMetadata {
    /// File modification time.
    pub mtime: SystemTime,
    /// File size in bytes.
    pub size: u64,
    /// Confidence level of this entry.
    pub confidence: Confidence,
    /// When this entry was last verified via stat.
    pub last_verified: Instant,
    /// Cached content hash (blake3, 32 bytes), if computed.
    pub content_hash: Option<[u8; 32]>,
    /// Unix `(ctime, inode)`; detects same-size, restored-mtime edits (#1943).
    pub identity: crate::identity::FileIdentity,
}

/// One entry selected by the read-only phase of metadata eviction.
///
/// The write phase must compare `last_verified` again while holding the
/// entry's shard write lock. A lookup may refresh the entry between the two
/// phases; in that case this candidate is stale and must not remove the newly
/// refreshed value.
#[derive(Debug, Clone)]
pub struct EvictionCandidate {
    path: NormalizedPath,
    last_verified: Instant,
}

impl EvictionCandidate {
    /// Path observed by the read-only candidate pass.
    #[must_use]
    pub fn path(&self) -> &NormalizedPath {
        &self.path
    }
}

/// Detailed result of conditionally deleting an eviction-candidate slice.
#[derive(Debug, Default)]
pub struct CandidateEvictionResult {
    /// Candidates removed with the same timestamp observed by the read pass.
    pub removed: usize,
    /// Candidates whose shard could not be acquired without blocking.
    pub busy: Vec<EvictionCandidate>,
    /// Candidates whose verification timestamp changed after the read pass.
    pub refreshed: Vec<EvictionCandidate>,
}

/// Concurrent file metadata cache.
///
/// Uses `DashMap` for sharded, concurrent access. Entries are keyed
/// by normalized path.
#[derive(Debug)]
pub struct MetadataCache {
    entries: DashMap<NormalizedPath, FileMetadata>,
    /// Duration after which a High-confidence entry decays to Medium.
    high_decay: Duration,
    /// Duration after which a Medium-confidence entry decays to Low.
    medium_decay: Duration,
    /// Set by every mutating method; cleared by [`Self::take_dirty`]. Lets a
    /// shutdown/periodic snapshot skip re-serializing and re-fsyncing
    /// `metadata.bin` when nothing has changed since the last save
    /// (zccache#1775). `merge_from` (background disk load) deliberately does
    /// not set this — the merged content already matches disk.
    dirty: AtomicBool,
}

impl FileMetadata {
    /// Whether two stats describe the same file state: `(mtime, size)` plus the
    /// unforgeable `(ctime, inode)` identity (#1943).
    #[must_use]
    pub fn same_stamp(&self, other: &Self) -> bool {
        self.mtime == other.mtime && self.size == other.size && self.identity == other.identity
    }
}

impl MetadataCache {
    /// Create a new metadata cache with default decay durations.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            high_decay: Duration::from_secs(60),
            medium_decay: Duration::from_secs(30),
            dirty: AtomicBool::new(false),
        }
    }

    /// Whether the cache has been mutated since the last [`Self::take_dirty`].
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    /// Clear the dirty flag, returning whether it was set. Callers check this
    /// immediately before a snapshot save to decide whether the save (and its
    /// serialize + fsync) is necessary at all.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }

    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Look up metadata for a path, applying confidence decay.
    ///
    /// Returns `None` if the path is not in the cache.
    #[must_use]
    pub fn get(&self, path: &NormalizedPath) -> Option<FileMetadata> {
        self.entries.get(path).map(|entry| {
            let mut meta = entry.clone();
            meta.confidence = self.decayed_confidence(&meta);
            meta
        })
    }

    /// Insert or update metadata for a path.
    pub fn insert(&self, path: NormalizedPath, metadata: FileMetadata) {
        self.entries.insert(path, metadata);
        self.mark_dirty();
    }

    /// Mark a path's entry as Low confidence (e.g., after watcher overflow).
    pub fn downgrade(&self, path: &NormalizedPath) {
        if let Some(mut entry) = self.entries.get_mut(path) {
            entry.confidence = Confidence::Low;
            self.mark_dirty();
        }
    }

    /// Downgrade all entries to Low confidence.
    pub fn downgrade_all(&self) {
        let mut any = false;
        for mut entry in self.entries.iter_mut() {
            entry.confidence = Confidence::Low;
            any = true;
        }
        if any {
            self.mark_dirty();
        }
    }

    /// Re-stat all Low-confidence entries and promote those whose
    /// filesystem metadata `(mtime, size)` still matches.
    ///
    /// Designed for overflow recovery: after a watcher overflow downgrades
    /// everything to Low, this method cheaply restores High confidence for
    /// files that haven't actually changed — avoiding unnecessary re-hashing
    /// on subsequent lookups.
    ///
    /// Returns the number of entries promoted back to High confidence.
    pub fn rescan_all(&self) -> usize {
        // Collect Low-confidence keys so we can stat them in parallel
        // without holding DashMap shard locks during I/O.
        let low_keys: Vec<NormalizedPath> = self
            .entries
            .iter()
            .filter(|e| e.confidence == Confidence::Low)
            .map(|e| e.key().clone())
            .collect();

        if low_keys.is_empty() {
            return 0;
        }

        // Parallel stat: each file is independent.
        let results: Vec<(NormalizedPath, FileMetadata)> = low_keys
            .par_iter()
            .filter_map(|path| {
                Self::stat_file(path)
                    .ok()
                    .map(|fresh| (path.clone(), fresh))
            })
            .collect();

        // Apply promotions back (fast, in-memory only).
        let mut promoted = 0;
        for (path, fresh) in results {
            if let Some(mut entry) = self.entries.get_mut(&path) {
                if entry.confidence == Confidence::Low && entry.same_stamp(&fresh) {
                    entry.confidence = Confidence::High;
                    entry.last_verified = Instant::now();
                    promoted += 1;
                }
            }
        }
        if promoted > 0 {
            self.mark_dirty();
        }
        promoted
    }

    /// Remove a path from the cache.
    pub fn remove(&self, path: &NormalizedPath) {
        if self.entries.remove(path).is_some() {
            self.mark_dirty();
        }
    }

    /// Returns the number of entries in the cache.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Remove all entries from the cache.
    pub fn clear(&self) {
        self.entries.clear();
        self.mark_dirty();
    }

    /// Drain entries from a freshly loaded `MetadataCache` into `self`
    /// using `DashMap::insert` (which is `&self`).
    ///
    /// Issue #784 phase 2b: lets a background `spawn_blocking` task load
    /// the on-disk `metadata.bin` snapshot AFTER the daemon has written
    /// its readiness lockfile, then populate the live cache without
    /// holding up bind. Readers during the merge window either see no
    /// entry (cold-path miss — safe; the next call to
    /// `get_cached_hash_if_stat_valid` re-stats and re-hashes if needed)
    /// or a loaded entry (the stat-verify safety net at the call site
    /// rejects a stale `(mtime, size)` before trusting the hash, so a
    /// partially-loaded snapshot cannot poison cache keys).
    pub fn merge_from(&self, other: Self) {
        for (k, v) in other.entries {
            self.entries.insert(k, v);
        }
    }

    /// Return a cached content hash if the entry has High or Medium confidence.
    ///
    /// This is the clock-aware fast path: the caller has journal-based
    /// assurance that the file hasn't changed, so time-based decay is
    /// not applied. Returns `None` if the entry is missing, Low confidence,
    /// or has no cached hash.
    #[must_use]
    pub fn get_cached_hash(&self, path: &NormalizedPath) -> Option<zccache_hash::ContentHash> {
        self.entries
            .get(path)
            .and_then(|entry| match entry.confidence {
                Confidence::High | Confidence::Medium => entry
                    .content_hash
                    .map(zccache_hash::ContentHash::from_bytes),
                Confidence::Low => None,
            })
    }

    /// Return a cached content hash only if `(mtime, size)` still match the
    /// filesystem — one `stat()` syscall, zero hashing.
    ///
    /// This is the safety net for the journal fast path: even when the journal
    /// reports no changes (watcher latency), a single stat catches files that
    /// were modified underneath us.
    ///
    /// Returns `None` if the entry is missing, Low confidence, has no cached
    /// hash, or the stat check fails / shows a mismatch.
    #[must_use]
    pub fn get_cached_hash_if_stat_valid(
        &self,
        path: &NormalizedPath,
    ) -> Option<zccache_hash::ContentHash> {
        // Copy the entry out under a short read guard so the stat below runs
        // with no shard guard alive — same lock discipline as `rescan_all`
        // and `fingerprint/verify.rs` (#1908, the #724 class).
        let (recorded_mtime, recorded_size, recorded_identity, hash) = {
            let entry = self.entries.get(path)?;
            match entry.confidence {
                Confidence::High | Confidence::Medium => {}
                Confidence::Low => return None,
            }
            let hash = entry
                .content_hash
                .map(zccache_hash::ContentHash::from_bytes)?;
            (entry.mtime, entry.size, entry.identity, hash)
        };

        // One stat syscall to verify mtime + size still match.
        let fs_meta = std::fs::metadata(path).ok()?;
        let mtime = fs_meta.modified().ok()?;
        let size = fs_meta.len();
        if mtimes_match(recorded_mtime, mtime)
            && recorded_size == size
            && recorded_identity == crate::identity::FileIdentity::from_metadata(&fs_meta)
        {
            Some(hash)
        } else {
            None
        }
    }

    /// Trim entries whose `last_verified` is older than `max_age`.
    /// Returns the number of entries removed.
    pub fn trim(&self, max_age: Duration) -> usize {
        let now = Instant::now();
        let mut removed = 0;
        self.entries.retain(|_, entry| {
            // Use saturating_duration_since to avoid panic if Instant is
            // non-monotonic (documented edge case on some platforms/VMs).
            if now.saturating_duration_since(entry.last_verified) > max_age {
                removed += 1;
                false
            } else {
                true
            }
        });
        if removed > 0 {
            self.mark_dirty();
        }
        removed
    }

    /// Evict the `count` oldest entries by `last_verified`.
    /// Returns the number actually removed (may be less than `count`).
    pub fn evict_oldest(&self, count: usize) -> usize {
        let candidates = self.oldest_eviction_candidates(count);
        self.evict_candidates(&candidates)
    }

    /// Collect the `count` oldest entries without taking any shard write lock.
    ///
    /// This is the read-only phase of two-pass eviction. The returned
    /// candidates are observations, not deletion authority: callers must use
    /// [`Self::evict_candidates`] or [`Self::try_evict_candidates`] so a
    /// refresh between the two phases is preserved.
    pub fn oldest_eviction_candidates(&self, count: usize) -> Vec<EvictionCandidate> {
        if count == 0 {
            return Vec::new();
        }
        // Collect (path, last_verified) then sort oldest first.
        let mut entries: Vec<EvictionCandidate> = self
            .entries
            .iter()
            .map(|entry| EvictionCandidate {
                path: entry.key().clone(),
                last_verified: entry.value().last_verified,
            })
            .collect();
        entries.sort_by_key(|candidate| candidate.last_verified);
        entries.truncate(count);
        entries
    }

    /// Remove candidates whose verification timestamp still matches the
    /// read-only selection pass.
    ///
    /// Each entry is revalidated while its shard write lock is held. Entries
    /// refreshed, replaced, or removed since candidate collection are skipped.
    pub fn evict_candidates(&self, candidates: &[EvictionCandidate]) -> usize {
        self.evict_candidates_detailed(candidates).removed
    }

    /// Detailed blocking write phase, including timestamp-refreshed entries.
    pub fn evict_candidates_detailed(
        &self,
        candidates: &[EvictionCandidate],
    ) -> CandidateEvictionResult {
        let mut result = CandidateEvictionResult::default();
        for candidate in candidates {
            if let Entry::Occupied(entry) = self.entries.entry(candidate.path.clone()) {
                if entry.get().last_verified == candidate.last_verified {
                    entry.remove();
                    result.removed += 1;
                } else {
                    result.refreshed.push(candidate.clone());
                }
            }
        }
        if result.removed > 0 {
            self.mark_dirty();
        }
        result
    }

    /// Nonblocking variant of [`Self::evict_candidates`].
    ///
    /// A candidate on a busy shard is skipped so active cache traffic wins.
    /// A later GC pass may retry it. Timestamp revalidation has the same
    /// semantics as the blocking variant.
    pub fn try_evict_candidates(&self, candidates: &[EvictionCandidate]) -> usize {
        self.try_evict_candidates_detailed(candidates).removed
    }

    /// Detailed nonblocking write phase, distinguishing lock contention from
    /// timestamp refreshes so a later forced pass can retry only busy entries.
    pub fn try_evict_candidates_detailed(
        &self,
        candidates: &[EvictionCandidate],
    ) -> CandidateEvictionResult {
        let mut result = CandidateEvictionResult::default();
        for candidate in candidates {
            let Some(entry) = self.entries.try_entry(candidate.path.clone()) else {
                result.busy.push(candidate.clone());
                continue;
            };
            if let Entry::Occupied(entry) = entry {
                if entry.get().last_verified == candidate.last_verified {
                    entry.remove();
                    result.removed += 1;
                } else {
                    result.refreshed.push(candidate.clone());
                }
            }
        }
        if result.removed > 0 {
            self.mark_dirty();
        }
        result
    }

    /// Iterate all cached paths.
    pub fn paths(&self) -> Vec<NormalizedPath> {
        self.entries.iter().map(|e| e.key().clone()).collect()
    }

    fn decayed_confidence(&self, meta: &FileMetadata) -> Confidence {
        // Use saturating_duration_since to avoid panic if Instant is
        // non-monotonic (documented edge case on some platforms/VMs).
        let elapsed = Instant::now().saturating_duration_since(meta.last_verified);
        match meta.confidence {
            Confidence::High if elapsed > self.high_decay => Confidence::Medium,
            Confidence::Medium if elapsed > self.medium_decay => Confidence::Low,
            other => other,
        }
    }
}

impl Default for MetadataCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "metadata_tests.rs"]
mod tests;
