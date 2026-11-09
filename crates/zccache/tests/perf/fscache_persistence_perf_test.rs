//! Perf regression tests for `MetadataCache` disk persistence.
//!
//! These exist because losing `MetadataCache` across daemon restarts
//! is invisible in unit tests yet catastrophic on the warm side of the
//! `cold-tar-untar-warm × medium` perf-rust-cluster cell: without
//! persistence, every fresh daemon (including the one spawned after
//! `soldr load` restores a cache dir) starts with an empty DashMap
//! and pays full stat+blake3 on every header lookup.
//!
//! Two regression-protecting assertions:
//!
//! 1. **`perf_metadata_save_load_roundtrip_under_50ms`** — the median
//!    load of a 200-entry snapshot stays under 50ms. If a future refactor
//!    accidentally introduces O(n^2) decode or per-entry syscalls,
//!    this fails loudly long before it shows up in a cluster run.
//!
//! 2. **`perf_metadata_load_enables_fast_path_after_restart`** —
//!    after `save → drop → load`, the
//!    `get_cached_hash_if_stat_valid` fast path still returns the
//!    cached hash for a real file. If a refactor breaks the
//!    confidence/clamp semantics so the safety net rejects every
//!    restored entry, this fails — that would silently revert the
//!    warm-side perf win.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unwrap_in_result
)]

use std::fs;
use std::time::{Duration, Instant, SystemTime};
use tempfile::TempDir;
use zccache::core::NormalizedPath;
use zccache::fscache::{Confidence, FileMetadata, MetadataCache};
use zccache_test_support::median;

/// Roughly the order of magnitude of headers a `medium` perf fixture
/// touches; large enough to surface algorithmic regressions, small
/// enough that the assertion is robust on CI runners.
const ENTRY_COUNT: usize = 200;

/// Conservative load budget. The actual load time is observed to be in
/// the low single-digit milliseconds on a developer laptop; 50ms gives
/// CI runners headroom while still catching pathological regressions.
const LOAD_BUDGET: Duration = Duration::from_millis(50);

/// Loads measured per run; the median must be within `LOAD_BUDGET`.
const LOAD_ROUNDS: usize = 31;

fn populate(cache: &MetadataCache, count: usize) {
    for i in 0..count {
        let key = NormalizedPath::from(format!("/virtual/perf/header_{i:04}.h"));
        cache.insert(
            key,
            FileMetadata {
                mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + i as u64),
                size: 4096 + i as u64,
                confidence: Confidence::High,
                last_verified: Instant::now(),
                hashed_at: None,
                identity: Default::default(),
                // 32-byte hash, deterministic per index so the bytes
                // round-trip predictably and we can spot decode bugs.
                content_hash: Some({
                    let mut h = [0u8; 32];
                    h[0] = (i & 0xff) as u8;
                    h[1] = ((i >> 8) & 0xff) as u8;
                    h
                }),
            },
        );
    }
}

#[test]
// regression test for the cold-tar-untar-warm × medium warm-side fast-path miss
fn perf_metadata_save_load_roundtrip_under_50ms() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("metadata.bin");

    let cache = MetadataCache::new();
    populate(&cache, ENTRY_COUNT);

    cache.save_to_disk(&path).expect("save_to_disk");
    drop(cache);

    // One wall-clock load fails whenever the host preempts it (#2081). The
    // median of many ignores those stalls and leaves the budget untouched,
    // while a cost paid by every load still moves the median past it.
    let mut samples = Vec::with_capacity(LOAD_ROUNDS);
    for _ in 0..LOAD_ROUNDS {
        let start = Instant::now();
        let loaded = MetadataCache::load_from_disk(&path).expect("load_from_disk");
        samples.push(start.elapsed());
        assert_eq!(loaded.len(), ENTRY_COUNT);
    }
    let elapsed = median(&mut samples);

    eprintln!(
        "perf_metadata_save_load_roundtrip_under_50ms: median load of {ENTRY_COUNT} entries \
         took {} ns over {LOAD_ROUNDS} rounds",
        elapsed.as_nanos()
    );

    assert!(
        elapsed < LOAD_BUDGET,
        "median metadata snapshot load took {} ns (budget {} ns); regression vs. the \
         warm-restart fast path",
        elapsed.as_nanos(),
        LOAD_BUDGET.as_nanos()
    );
}

#[test]
// regression test for the cold-tar-untar-warm × medium warm-side fast-path miss
fn perf_metadata_load_enables_fast_path_after_restart() {
    let dir = TempDir::new().unwrap();
    let metadata_path = dir.path().join("metadata.bin");

    // Create a real file so the stat-verify safety net inside
    // `get_cached_hash_if_stat_valid` has something to compare against.
    let file_path = dir.path().join("header.h");
    fs::write(&file_path, b"#pragma once\nint x = 1;\n").unwrap();
    // Stat-only reuse is valid only after the write's timestamp tick (#1989).
    // Use a stable old mtime instead of depending on runner scheduling.
    kernal_api::platform::fs::set_file_mtime(
        &file_path,
        kernal_api::platform::fs::FileTime::from_system_time(
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_000),
        ),
    )
    .unwrap();
    let normalized = NormalizedPath::from(file_path.as_path());

    // Populate via the real lookup path so (mtime, size, hash) match
    // the on-disk file exactly — the round-trip is testing that the
    // post-load entry survives the stat check.
    let cache = MetadataCache::new();
    let initial_hash = cache.lookup(file_path.as_path()).expect("lookup");

    cache.save_to_disk(&metadata_path).expect("save_to_disk");
    drop(cache);

    // Fresh process boundary: nothing in memory, nothing in the new
    // cache's DashMap until load_from_disk repopulates it.
    let restored = MetadataCache::load_from_disk(&metadata_path).expect("load_from_disk");
    assert_eq!(restored.len(), 1, "exactly one entry restored");

    let recovered = restored.get_cached_hash_if_stat_valid(&normalized).expect(
        "post-restart fast path missed — this is the warm-side regression the perf-cluster catches",
    );
    assert_eq!(
        *recovered.as_bytes(),
        *initial_hash.as_bytes(),
        "restored hash diverged from original — persistence is corrupting content_hash",
    );
}
