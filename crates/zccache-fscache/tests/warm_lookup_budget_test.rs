//! Warm-hit budget for the stat fast path (#1943 overhead guard).
//!
//! The `(ctime, inode)` identity is read from the stat the fast path already
//! performs, so a warm `lookup_since` must stay one stat and zero hashing.
//! Prints the per-lookup cost so the before/after can be compared.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};
use zccache_core::NormalizedPath;
use zccache_fscache::CacheSystem;

#[test]
fn warm_lookup_since_stays_one_stat() {
    const FILES: usize = 500;
    const ROUNDS: usize = 20;
    let dir = tempfile::tempdir().unwrap();
    // 64 KiB files: a re-hash per lookup would blow the budget by orders of magnitude.
    let body = vec![b'x'; 64 * 1024];
    let paths: Vec<NormalizedPath> = (0..FILES)
        .map(|i| {
            let p = dir.path().join(format!("src{i}.rs"));
            std::fs::write(&p, &body).unwrap();
            NormalizedPath::from(p.as_path())
        })
        .collect();
    let cache = CacheSystem::new();
    for p in &paths {
        cache.lookup_since(p, zccache_fscache::Clock::ZERO).unwrap();
    }
    let clock = cache.current_clock();
    let start = Instant::now();
    for _ in 0..ROUNDS {
        for p in &paths {
            cache.lookup_since(p, clock).unwrap();
        }
    }
    let elapsed = start.elapsed();
    let per_lookup_ns = elapsed.as_nanos() / (FILES * ROUNDS) as u128;
    eprintln!(
        "warm lookup_since: {per_lookup_ns} ns/lookup over {} lookups",
        FILES * ROUNDS
    );
    // 10k one-stat lookups; generous for slow CI disks, far below 10k 64 KiB hashes.
    assert!(
        elapsed < Duration::from_secs(2),
        "warm lookups took {elapsed:?} ({per_lookup_ns} ns/lookup)"
    );
}
