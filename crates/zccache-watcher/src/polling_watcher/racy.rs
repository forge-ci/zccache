//! Bounded content verification for ambiguous polling baselines (#2169).

use super::*;
use std::time::{SystemTime, UNIX_EPOCH};
use zccache_core::file_timestamp::HASH_TIMESTAMP_GRANULARITY;
use zccache_hash::{hash_reader, ContentHash};

/// At most 256 content reads per poll, including initial baseline capture.
/// Deferred entries retain their digest and get priority on the next poll.
pub(super) const HASHES_PER_POLL: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RacyState {
    hash: Option<ContentHash>,
    deferred_polls: u64,
}

fn is_racy(mtime_ns: u128, observed_at: SystemTime) -> bool {
    observed_at
        .duration_since(UNIX_EPOCH)
        .ok()
        .is_none_or(|age| {
            mtime_ns
                >= age
                    .as_nanos()
                    .saturating_sub(HASH_TIMESTAMP_GRANULARITY.as_nanos())
        })
}

/// Scan errors preserve the caller's last complete baseline, including digests.
pub(super) fn poll_snapshot(
    config: &ScanConfig,
    parallelism: DirectoryWalkParallelism,
    previous: &HashMap<NormalizedPath, FileState>,
) -> std::io::Result<(HashMap<NormalizedPath, FileState>, HashSet<NormalizedPath>)> {
    let observed_at = SystemTime::now();
    let mut current = scan_snapshot(config, parallelism)?;
    let (changed, _) = verify(previous, &mut current, observed_at, HASHES_PER_POLL)?;
    Ok((current, changed))
}

pub(super) fn verify(
    previous: &HashMap<NormalizedPath, FileState>,
    current: &mut HashMap<NormalizedPath, FileState>,
    observed_at: SystemTime,
    limit: usize,
) -> std::io::Result<(HashSet<NormalizedPath>, usize)> {
    let mut candidates = Vec::new();
    for (path, state) in current {
        let old = previous.get(path).filter(|old| old.same_stat(state));
        let prior = old.and_then(|old| old.racy.as_ref());
        if prior.is_some() || is_racy(state.mtime_ns, observed_at) {
            state.racy = Some(prior.cloned().unwrap_or(RacyState {
                hash: None,
                deferred_polls: 0,
            }));
            candidates.push((path, state, old.is_some()));
        }
    }
    // Previously deferred work precedes newly racy entries; sorting paths makes
    // selection deterministic and counting skipped polls prevents starvation.
    candidates.sort_unstable_by(|a, b| {
        b.1.racy
            .as_ref()
            .map(|r| r.deferred_polls)
            .cmp(&a.1.racy.as_ref().map(|r| r.deferred_polls))
            .then_with(|| a.0.cmp(b.0))
    });
    let mut changed = HashSet::new();
    let mut reads = 0;
    for (path, state, existed) in candidates {
        if reads >= limit {
            if let Some(racy) = state.racy.as_mut() {
                racy.deferred_polls = racy.deferred_polls.saturating_add(1);
            }
            continue;
        }
        reads += 1;
        // Streaming avoids mmap hazards when a concurrently edited file shrinks.
        let hash = hash_reader(std::fs::File::open(path)?)?;
        if !state.same_stat(&file_state(&std::fs::metadata(path)?)) {
            return Err(std::io::Error::other(format!(
                "{} changed while hashing",
                path.display()
            )));
        }
        let Some(racy) = state.racy.as_mut() else {
            continue;
        };
        if racy.hash.is_some_and(|old| old != hash) || (existed && racy.hash.is_none()) {
            // A capped baseline has no digest to prove unchanged bytes. Report
            // conservatively once when it is finally checked, never lose it.
            changed.insert(path.clone());
        }
        if is_racy(state.mtime_ns, observed_at) {
            racy.hash = Some(hash);
            racy.deferred_polls = 0;
        } else {
            state.racy = None;
        }
    }
    Ok((changed, reads))
}
