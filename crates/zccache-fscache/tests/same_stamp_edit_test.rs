//! Regression for zccache#1943: an in-place source edit that keeps the
//! file size and restores the original mtime must never be served the
//! stale cached content hash (the rustc stale-HIT false green).
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::time::SystemTime;
use zccache_core::NormalizedPath;
use zccache_fscache::{CacheSystem, Clock, MetadataCache};

/// Rewrite `path` in place with same-size `content`, then put the original
/// mtime back exactly — what `sed -i` + `touch -d` (or a rebase) does.
fn same_stamp_edit(path: &std::path::Path, content: &[u8]) {
    let before = fs::metadata(path).unwrap();
    assert_eq!(before.len(), content.len() as u64, "edit must keep size");
    let mtime: SystemTime = before.modified().unwrap();
    // Same inode: write through the existing file, like an editor save.
    fs::write(path, content).unwrap();
    let f = fs::File::options().write(true).open(path).unwrap();
    f.set_modified(mtime).unwrap();
    drop(f);
    let after = fs::metadata(path).unwrap();
    assert_eq!(after.modified().unwrap(), mtime);
    assert_eq!(after.len(), before.len());
}

#[test]
fn lookup_since_rehashes_same_size_same_mtime_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lib.rs");
    fs::write(&path, b"//! ok\nfn main() {}\n").unwrap();
    let np = NormalizedPath::from(path.as_path());

    let cache = CacheSystem::new();
    let first = cache.lookup_since(&np, Clock::ZERO).unwrap().hash;
    // Unchanged replay must still hit the same hash.
    let clock = cache.current_clock();
    assert_eq!(cache.lookup_since(&np, clock).unwrap().hash, first);

    same_stamp_edit(&path, b"cp! ok\nfn main() {}\n");
    let clock = cache.current_clock();
    let second = cache.lookup_since(&np, clock).unwrap().hash;
    assert_ne!(
        first, second,
        "stale hash served for edited content (#1943)"
    );
    assert_eq!(second, zccache_hash::hash_file(&path).unwrap());
}

#[test]
fn metadata_lookup_rehashes_same_size_same_mtime_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lib.rs");
    fs::write(&path, b"//! ok\n").unwrap();
    let cache = MetadataCache::new();
    let first = cache.lookup(&path).unwrap();
    same_stamp_edit(&path, b"cp! ok\n");
    assert_ne!(cache.lookup(&path).unwrap(), first, "#1943 stale hash");
}

#[test]
fn persisted_snapshot_rejects_same_size_same_mtime_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lib.rs");
    let snap = dir.path().join("metadata.bin");
    fs::write(&path, b"//! ok\n").unwrap();
    let cache = MetadataCache::new();
    let first = cache.lookup(&path).unwrap();
    cache.save_to_disk(&snap).unwrap();

    same_stamp_edit(&path, b"cp! ok\n");
    // A fresh (embedded) daemon loads the snapshot and must not trust it.
    let loaded = MetadataCache::load_from_disk(&snap).unwrap();
    let np = NormalizedPath::from(path.as_path());
    assert_ne!(loaded.get_cached_hash_if_stat_valid(&np), Some(first));
    assert_ne!(loaded.lookup(&path).unwrap(), first, "#1943 stale hash");
}
