use super::*;
use kernal_api::platform::fs::{set_file_mtime, FileTime};
use std::collections::HashSet;
use std::fs;

// ── mtime tolerance (issue #469) ─────────────────────────────────────

fn ts(secs: u64, nanos: u32) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::new(secs, nanos)
}

#[test]
fn default_mtime_tolerance_is_one_second() {
    // Lock in the documented default. The constant is `pub` so consumers
    // and tests both depend on this being 1_000_000_000 ns.
    assert_eq!(DEFAULT_MTIME_TOLERANCE_NS, 1_000_000_000);
}

#[test]
fn mtimes_match_byte_equal() {
    assert!(mtimes_match(ts(1_000, 0), ts(1_000, 0)));
    assert!(mtimes_match(ts(1_000, 500_000), ts(1_000, 500_000)));
}

#[test]
fn mtimes_match_archive_truncation_one_side_zero() {
    // Cached side has sub-second precision; archive-restored side lost
    // it. Drift is ~141 ms, well within the 1 s default tolerance.
    assert!(mtimes_match(ts(1_000, 141_490_982), ts(1_000, 0)));
    // Symmetric — either side can be the truncated one.
    assert!(mtimes_match(ts(1_000, 0), ts(1_000, 141_490_982)));
}

#[test]
fn mtimes_match_only_with_archive_truncation_signature() {
    // A timestamp at a whole second can be an archive-restored version of
    // the other timestamp, so the sub-second drift is tolerated.
    assert!(mtimes_match(ts(1_000, 0), ts(1_000, 500_000_000)));
    assert!(mtimes_match(ts(1_000, 0), ts(1_000, 999_999_999)));

    // Fine-grained timestamps are exact, even when the difference is
    // much smaller than the one-second archive tolerance.
    assert!(!mtimes_match(
        ts(1_000, 100_000_000),
        ts(1_000, 500_000_000)
    ));

    // The tolerance is strictly less than one second.
    assert!(!mtimes_match(ts(1_000, 0), ts(1_001, 0)));
}

#[test]
fn mtimes_dont_match_beyond_default_tolerance() {
    // Drift = 1.1 s (100 ms past the 1 s boundary). Picked at 100 ms
    // rather than 1 ns past the boundary because Windows `SystemTime`
    // uses 100 ns FILETIME resolution — anything finer than 100 ns gets
    // truncated and the test would compare equal across the boundary.
    assert!(!mtimes_match(ts(1_000, 0), ts(1_001, 100_000_000)));
    // Multi-second drift — clearly a real change on every platform.
    assert!(!mtimes_match(ts(1_000, 0), ts(1_002, 0)));
    assert!(!mtimes_match(ts(1_000, 500_000), ts(1_010, 0)));
}

#[test]
fn mtimes_match_pre_epoch_safe() {
    // Pre-UNIX-EPOCH times resolve to (0, 0) via mtime_components — should
    // not panic, should compare equal to other pre-epoch values.
    let pre = SystemTime::UNIX_EPOCH - Duration::from_secs(1);
    assert!(mtimes_match(pre, pre));
}

#[test]
fn stat_fast_path_rejects_same_size_fine_grained_mtime_change() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("input.c");
    fs::write(&file, b"old!").unwrap();

    let cached_mtime = ts(1_000, 100_000_000);
    let fresh_mtime = ts(1_000, 500_000_000);
    set_file_mtime(&file, FileTime::from_system_time(fresh_mtime)).unwrap();

    let path = NormalizedPath::from(file.as_path());
    let cache = MetadataCache::new();
    cache.insert(
        path.clone(),
        FileMetadata {
            mtime: cached_mtime,
            size: 4,
            confidence: Confidence::High,
            last_verified: Instant::now(),
            content_hash: Some([42; 32]),
            identity: Default::default(),
        },
    );

    // A watcher event may not have arrived yet, but the stat safety net
    // must not return the stale hash for a same-size native-resolution
    // timestamp change within the former one-second blanket window.
    assert!(cache.get_cached_hash_if_stat_valid(&path).is_none());
}

#[test]
fn stat_fast_path_accepts_archive_truncated_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("input.c");
    fs::write(&file, b"same").unwrap();

    let cached_mtime = ts(1_000, 141_490_982);
    set_file_mtime(&file, FileTime::from_system_time(ts(1_000, 0))).unwrap();

    let path = NormalizedPath::from(file.as_path());
    let cache = MetadataCache::new();
    cache.insert(
        path.clone(),
        FileMetadata {
            mtime: cached_mtime,
            size: 4,
            confidence: Confidence::High,
            last_verified: Instant::now(),
            content_hash: Some([42; 32]),
            identity: crate::identity::FileIdentity::from_metadata(
                &std::fs::metadata(&file).unwrap(),
            ),
        },
    );

    assert!(cache.get_cached_hash_if_stat_valid(&path).is_some());
}

#[test]
fn insert_and_get() {
    let cache = MetadataCache::new();
    let path = NormalizedPath::from("/tmp/test.c");
    let meta = FileMetadata {
        mtime: SystemTime::now(),
        size: 100,
        confidence: Confidence::High,
        last_verified: Instant::now(),
        content_hash: None,
        identity: Default::default(),
    };
    cache.insert(path.clone(), meta);
    assert!(cache.get(&path).is_some());
}

#[test]
fn get_returns_none_for_missing_path() {
    let cache = MetadataCache::new();
    assert!(cache.get(&NormalizedPath::from("/no/such/path")).is_none());
}

#[test]
fn insert_overwrites_existing() {
    let cache = MetadataCache::new();
    let path = NormalizedPath::from("/tmp/overwrite.c");

    let meta1 = FileMetadata {
        mtime: SystemTime::now(),
        size: 100,
        confidence: Confidence::High,
        last_verified: Instant::now(),
        content_hash: None,
        identity: Default::default(),
    };
    cache.insert(path.clone(), meta1);
    assert_eq!(cache.get(&path).unwrap().size, 100);

    let meta2 = FileMetadata {
        mtime: SystemTime::now(),
        size: 999,
        confidence: Confidence::Medium,
        last_verified: Instant::now(),
        content_hash: None,
        identity: Default::default(),
    };
    cache.insert(path.clone(), meta2);
    assert_eq!(cache.get(&path).unwrap().size, 999);
}

#[test]
fn downgrade_single_path() {
    let cache = MetadataCache::new();
    let path_a = NormalizedPath::from("/tmp/a.c");
    let path_b = NormalizedPath::from("/tmp/b.c");

    for path in [&path_a, &path_b] {
        cache.insert(
            path.clone(),
            FileMetadata {
                mtime: SystemTime::now(),
                size: 10,
                confidence: Confidence::High,
                last_verified: Instant::now(),
                content_hash: None,
                identity: Default::default(),
            },
        );
    }

    cache.downgrade(&path_a);
    assert_eq!(cache.get(&path_a).unwrap().confidence, Confidence::Low);
    assert_eq!(cache.get(&path_b).unwrap().confidence, Confidence::High);
}

#[test]
fn downgrade_nonexistent_is_noop() {
    let cache = MetadataCache::new();
    cache.downgrade(&NormalizedPath::from("/no/such/path")); // should not panic
}

#[test]
fn remove_entry() {
    let cache = MetadataCache::new();
    let path = NormalizedPath::from("/tmp/removable.c");
    cache.insert(
        path.clone(),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::High,
            last_verified: Instant::now(),
            content_hash: None,
            identity: Default::default(),
        },
    );
    assert_eq!(cache.len(), 1);

    cache.remove(&path);
    assert!(cache.get(&path).is_none());
    assert_eq!(cache.len(), 0);
}

#[test]
fn remove_nonexistent_is_noop() {
    let cache = MetadataCache::new();
    cache.remove(&NormalizedPath::from("/no/such/path")); // should not panic
}

#[test]
fn is_empty_and_len() {
    let cache = MetadataCache::new();
    assert!(cache.is_empty());
    assert_eq!(cache.len(), 0);

    cache.insert(
        NormalizedPath::from("/tmp/x.c"),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 1,
            confidence: Confidence::High,
            last_verified: Instant::now(),
            content_hash: None,
            identity: Default::default(),
        },
    );
    assert!(!cache.is_empty());
    assert_eq!(cache.len(), 1);
}

#[test]
fn get_cached_hash_high_confidence() {
    let cache = MetadataCache::new();
    let path = NormalizedPath::from("/tmp/hashed.c");
    let hash_bytes = [42u8; 32];
    cache.insert(
        path.clone(),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::High,
            last_verified: Instant::now(),
            content_hash: Some(hash_bytes),
            identity: Default::default(),
        },
    );

    let result = cache.get_cached_hash(&path);
    assert!(result.is_some());
    assert_eq!(*result.unwrap().as_bytes(), hash_bytes);
}

#[test]
fn get_cached_hash_medium_confidence() {
    let cache = MetadataCache::new();
    let path = NormalizedPath::from("/tmp/med.c");
    let hash_bytes = [7u8; 32];
    cache.insert(
        path.clone(),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::Medium,
            last_verified: Instant::now(),
            content_hash: Some(hash_bytes),
            identity: Default::default(),
        },
    );

    let result = cache.get_cached_hash(&path);
    assert!(result.is_some());
}

#[test]
fn get_cached_hash_low_confidence_returns_none() {
    let cache = MetadataCache::new();
    let path = NormalizedPath::from("/tmp/low.c");
    cache.insert(
        path.clone(),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::Low,
            last_verified: Instant::now(),
            content_hash: Some([1u8; 32]),
            identity: Default::default(),
        },
    );

    assert!(cache.get_cached_hash(&path).is_none());
}

#[test]
fn get_cached_hash_no_hash_returns_none() {
    let cache = MetadataCache::new();
    let path = NormalizedPath::from("/tmp/nohash.c");
    cache.insert(
        path.clone(),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::High,
            last_verified: Instant::now(),
            content_hash: None,
            identity: Default::default(),
        },
    );

    assert!(cache.get_cached_hash(&path).is_none());
}

#[test]
fn get_cached_hash_missing_path_returns_none() {
    let cache = MetadataCache::new();
    assert!(cache
        .get_cached_hash(&NormalizedPath::from("/no/such"))
        .is_none());
}

#[test]
fn clear_removes_all_entries() {
    let cache = MetadataCache::new();
    for i in 0..5 {
        cache.insert(
            NormalizedPath::from(format!("/tmp/clear{i}.c")),
            FileMetadata {
                mtime: SystemTime::now(),
                size: i as u64,
                confidence: Confidence::High,
                last_verified: Instant::now(),
                content_hash: None,
                identity: Default::default(),
            },
        );
    }
    assert_eq!(cache.len(), 5);
    cache.clear();
    assert!(cache.is_empty());
}

#[test]
fn trim_removes_old_entries() {
    let cache = MetadataCache::new();
    let old = Instant::now() - Duration::from_secs(120);
    cache.insert(
        NormalizedPath::from("/tmp/old.c"),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::High,
            last_verified: old,
            content_hash: None,
            identity: Default::default(),
        },
    );
    cache.insert(
        NormalizedPath::from("/tmp/new.c"),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::High,
            last_verified: Instant::now(),
            content_hash: None,
            identity: Default::default(),
        },
    );
    let removed = cache.trim(Duration::from_secs(60));
    assert_eq!(removed, 1);
    assert_eq!(cache.len(), 1);
    assert!(cache.get(&NormalizedPath::from("/tmp/old.c")).is_none());
    assert!(cache.get(&NormalizedPath::from("/tmp/new.c")).is_some());
}

#[test]
fn trim_keeps_recent_entries() {
    let cache = MetadataCache::new();
    for i in 0..5 {
        cache.insert(
            NormalizedPath::from(format!("/tmp/recent{i}.c")),
            FileMetadata {
                mtime: SystemTime::now(),
                size: 10,
                confidence: Confidence::High,
                last_verified: Instant::now(),
                content_hash: None,
                identity: Default::default(),
            },
        );
    }
    let removed = cache.trim(Duration::from_secs(60));
    assert_eq!(removed, 0);
    assert_eq!(cache.len(), 5);
}

#[test]
fn evict_oldest_removes_n() {
    let cache = MetadataCache::new();
    let base = Instant::now() - Duration::from_secs(100);
    for i in 0..5 {
        cache.insert(
            NormalizedPath::from(format!("/tmp/e{i}.c")),
            FileMetadata {
                mtime: SystemTime::now(),
                size: 10,
                confidence: Confidence::High,
                last_verified: base + Duration::from_secs(i * 10),
                content_hash: None,
                identity: Default::default(),
            },
        );
    }
    let removed = cache.evict_oldest(2);
    assert_eq!(removed, 2);
    assert_eq!(cache.len(), 3);
}

#[test]
fn eviction_candidate_preserves_entry_refreshed_after_read_pass() {
    let cache = MetadataCache::new();
    let path = NormalizedPath::from("/tmp/refreshed.c");
    let selected_at = Instant::now() - Duration::from_secs(120);
    cache.insert(
        path.clone(),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::High,
            last_verified: selected_at,
            content_hash: None,
            identity: Default::default(),
        },
    );

    let candidates = cache.oldest_eviction_candidates(1);
    assert_eq!(candidates.len(), 1);

    let refreshed_at = selected_at + Duration::from_secs(60);
    cache.insert(
        path.clone(),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::High,
            last_verified: refreshed_at,
            content_hash: None,
            identity: Default::default(),
        },
    );

    assert_eq!(cache.evict_candidates(&candidates), 0);
    assert_eq!(cache.get(&path).unwrap().last_verified, refreshed_at);
}

#[test]
fn nonblocking_eviction_revalidates_candidate_timestamp() {
    let cache = MetadataCache::new();
    let path = NormalizedPath::from("/tmp/refreshed-try.c");
    let selected_at = Instant::now() - Duration::from_secs(120);
    cache.insert(
        path.clone(),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::High,
            last_verified: selected_at,
            content_hash: None,
            identity: Default::default(),
        },
    );

    let candidates = cache.oldest_eviction_candidates(1);
    cache.insert(
        path.clone(),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::High,
            last_verified: selected_at + Duration::from_secs(1),
            content_hash: None,
            identity: Default::default(),
        },
    );

    assert_eq!(cache.try_evict_candidates(&candidates), 0);
    assert!(cache.get(&path).is_some());
}

#[test]
fn nonblocking_eviction_reports_busy_candidate_for_forced_retry() {
    let cache = MetadataCache::new();
    let path = NormalizedPath::from("/tmp/busy-try.c");
    cache.insert(
        path.clone(),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::High,
            last_verified: Instant::now(),
            content_hash: None,
            identity: Default::default(),
        },
    );
    let candidates = cache.oldest_eviction_candidates(1);
    let shard_guard = cache.entries.get_mut(&path).unwrap();

    let gentle = cache.try_evict_candidates_detailed(&candidates);
    assert_eq!(gentle.removed, 0);
    assert_eq!(gentle.busy.len(), 1);
    assert!(gentle.refreshed.is_empty());
    drop(shard_guard);

    let forced = cache.evict_candidates_detailed(&gentle.busy);
    assert_eq!(forced.removed, 1);
    assert!(cache.get(&path).is_none());
}

#[test]
fn evict_oldest_zero_noop() {
    let cache = MetadataCache::new();
    cache.insert(
        NormalizedPath::from("/tmp/z.c"),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::High,
            last_verified: Instant::now(),
            content_hash: None,
            identity: Default::default(),
        },
    );
    let removed = cache.evict_oldest(0);
    assert_eq!(removed, 0);
    assert_eq!(cache.len(), 1);
}

#[test]
fn evict_oldest_exceeds_count() {
    let cache = MetadataCache::new();
    cache.insert(
        NormalizedPath::from("/tmp/only.c"),
        FileMetadata {
            mtime: SystemTime::now(),
            size: 10,
            confidence: Confidence::High,
            last_verified: Instant::now(),
            content_hash: None,
            identity: Default::default(),
        },
    );
    let removed = cache.evict_oldest(100);
    assert_eq!(removed, 1);
    assert!(cache.is_empty());
}

#[test]
fn paths_returns_all() {
    let cache = MetadataCache::new();
    let expected: HashSet<NormalizedPath> = (0..3)
        .map(|i| NormalizedPath::from(format!("/tmp/p{i}.c")))
        .collect();
    for p in &expected {
        cache.insert(
            p.clone(),
            FileMetadata {
                mtime: SystemTime::now(),
                size: 10,
                confidence: Confidence::High,
                last_verified: Instant::now(),
                content_hash: None,
                identity: Default::default(),
            },
        );
    }
    let actual: HashSet<NormalizedPath> = cache.paths().into_iter().collect();
    assert_eq!(actual, expected);
}

#[test]
fn confidence_ordering() {
    assert!(Confidence::Low < Confidence::Medium);
    assert!(Confidence::Medium < Confidence::High);
    assert!(Confidence::Low < Confidence::High);
}

#[test]
fn default_creates_new_cache() {
    let cache = MetadataCache::default();
    assert!(cache.is_empty());
}

#[test]
fn downgrade_all_works() {
    let cache = MetadataCache::new();
    for i in 0..10 {
        let path = NormalizedPath::from(format!("/tmp/test{i}.c"));
        let meta = FileMetadata {
            mtime: SystemTime::now(),
            size: 100,
            confidence: Confidence::High,
            last_verified: Instant::now(),
            content_hash: None,
            identity: Default::default(),
        };
        cache.insert(path, meta);
    }
    cache.downgrade_all();
    // After downgrade, all entries should be Low confidence
    for entry in cache.entries.iter() {
        assert_eq!(entry.confidence, Confidence::Low);
    }
}

// zccache#1775: dirty-flag tracking lets a shutdown/periodic snapshot
// skip metadata.bin when nothing has changed since the last save.
mod dirty_flag {
    use super::*;

    fn meta() -> FileMetadata {
        FileMetadata {
            mtime: SystemTime::now(),
            size: 1,
            confidence: Confidence::High,
            last_verified: Instant::now(),
            content_hash: Some([0x42; 32]),
            identity: Default::default(),
        }
    }

    #[test]
    fn starts_clean() {
        let cache = MetadataCache::new();
        assert!(!cache.is_dirty());
        assert!(!cache.take_dirty());
    }

    #[test]
    fn insert_marks_dirty_until_taken() {
        let cache = MetadataCache::new();
        cache.insert(NormalizedPath::new("/a"), meta());
        assert!(cache.is_dirty());
        assert!(cache.take_dirty());
        assert!(!cache.is_dirty());
    }

    #[test]
    fn remove_of_missing_path_does_not_mark_dirty() {
        let cache = MetadataCache::new();
        cache.remove(&NormalizedPath::new("/missing"));
        assert!(!cache.is_dirty());
    }

    #[test]
    fn merge_from_does_not_mark_dirty() {
        let loaded = MetadataCache::new();
        loaded.insert(NormalizedPath::new("/a"), meta());

        let live = MetadataCache::new();
        live.merge_from(loaded);

        assert!(
            !live.is_dirty(),
            "a background disk load must not require an immediate re-save"
        );
    }
}
