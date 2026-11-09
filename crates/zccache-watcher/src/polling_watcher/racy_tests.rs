#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::tempdir;

fn set_mtime(path: &Path, mtime: SystemTime) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
}

fn scan(root: &Path) -> HashMap<NormalizedPath, FileState> {
    let config = build_config(root, &[], &["**/*.cpp".into()], &[]).unwrap();
    scan_snapshot(&config, DirectoryWalkParallelism::SharedPool).unwrap()
}

#[test]
fn same_tick_same_size_rewrite_is_reported() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("input.cpp");
    fs::write(&path, b"aaaa").unwrap();
    let mtime = fs::metadata(&path).unwrap().modified().unwrap();
    let config = build_config(dir.path(), &[], &["**/*.cpp".into()], &[]).unwrap();
    let mut previous = scan_snapshot(&config, DirectoryWalkParallelism::SharedPool).unwrap();
    racy::verify(&HashMap::new(), &mut previous, mtime, racy::HASHES_PER_POLL).unwrap();
    fs::write(&path, b"bbbb").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let mut current = scan_snapshot(&config, DirectoryWalkParallelism::SharedPool).unwrap();
    let (changed, reads) = racy::verify(
        &previous,
        &mut current,
        mtime + Duration::from_secs(3),
        racy::HASHES_PER_POLL,
    )
    .unwrap();
    let (_, removed) = diff_snapshots(&previous, &current);
    assert_eq!(reads, 1);
    assert!(current.values().all(|state| state.racy.is_none()));
    assert!(removed.is_empty());
    assert!(
        changed.contains(&NormalizedPath::new(&path)),
        "same-tick rewrite was lost"
    );
}

#[test]
fn public_watcher_reports_restored_mtime_rewrite() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("input.cpp");
    fs::write(&path, b"aaaa").unwrap();
    // Future mtime keeps this end-to-end test racy even on a loaded runner.
    let mtime = SystemTime::now() + Duration::from_secs(60);
    set_mtime(&path, mtime);
    let mut config = PollingWatcherConfig::new(dir.path());
    config.poll_interval = Duration::from_millis(10);
    config.debounce = Duration::from_millis(10);
    let watcher = PollingWatcher::new(config).unwrap();
    watcher.start().unwrap();
    fs::write(&path, b"bbbb").unwrap();
    set_mtime(&path, mtime);
    let batch = watcher
        .poll_timeout(Duration::from_secs(15))
        .unwrap()
        .expect("rewrite batch");
    assert!(batch.changed.contains(&NormalizedPath::new(&path)));
    watcher.stop().unwrap();
}

#[test]
fn unchanged_racy_digest_is_verified_before_retirement() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("input.cpp");
    fs::write(&path, b"aaaa").unwrap();
    let mtime = fs::metadata(&path).unwrap().modified().unwrap();
    let mut previous = scan(dir.path());
    racy::verify(&HashMap::new(), &mut previous, mtime, 256).unwrap();
    let mut current = scan(dir.path());
    let (changed, reads) =
        racy::verify(&previous, &mut current, mtime + Duration::from_secs(3), 256).unwrap();
    assert!(changed.is_empty());
    assert_eq!(reads, 1);
    assert!(current.values().all(|state| state.racy.is_none()));
    let mut later = scan(dir.path());
    assert_eq!(
        racy::verify(&current, &mut later, mtime + Duration::from_secs(4), 256)
            .unwrap()
            .1,
        0
    );
}

#[test]
fn capped_baselines_remain_pending_and_rotate_without_starvation() {
    let dir = tempdir().unwrap();
    let now = SystemTime::now();
    // Three cohorts exercise deferral across more than just the next poll.
    let count = racy::HASHES_PER_POLL * 2 + 1;
    for index in 0..count {
        let path = dir.path().join(format!("input-{index:04}.cpp"));
        fs::write(&path, b"aaaa").unwrap();
        set_mtime(&path, now);
    }
    let mut previous = scan(dir.path());
    assert_eq!(
        racy::verify(&HashMap::new(), &mut previous, now, racy::HASHES_PER_POLL)
            .unwrap()
            .1,
        racy::HASHES_PER_POLL
    );
    let mut checked = HashSet::new();
    for _ in 0..3 {
        let mut current = scan(dir.path());
        let (changed, reads) = racy::verify(
            &previous,
            &mut current,
            now + Duration::from_secs(3),
            racy::HASHES_PER_POLL,
        )
        .unwrap();
        assert!(reads <= racy::HASHES_PER_POLL);
        checked.extend(changed);
        previous = current;
    }
    assert_eq!(checked.len(), count - racy::HASHES_PER_POLL);
    assert!(previous.values().all(|state| state.racy.is_none()));
}

#[test]
fn persistent_racy_entries_cannot_starve_behind_the_cap() {
    let dir = tempdir().unwrap();
    let now = SystemTime::now();
    for name in ["a.cpp", "b.cpp", "c.cpp"] {
        let path = dir.path().join(name);
        fs::write(&path, b"aaaa").unwrap();
        set_mtime(&path, now);
    }
    let mut previous = scan(dir.path());
    racy::verify(&HashMap::new(), &mut previous, now, 1).unwrap();
    let mut checked = HashSet::new();
    for _ in 0..4 {
        let mut current = scan(dir.path());
        let (changed, reads) = racy::verify(&previous, &mut current, now, 1).unwrap();
        assert_eq!(reads, 1);
        checked.extend(changed);
        previous = current;
    }
    assert!(checked.contains(&NormalizedPath::new(dir.path().join("c.cpp"))));
    let path = dir.path().join("c.cpp");
    fs::write(&path, b"bbbb").unwrap();
    set_mtime(&path, now);
    let mut changes = HashSet::new();
    for _ in 0..3 {
        let mut current = scan(dir.path());
        changes.extend(racy::verify(&previous, &mut current, now, 1).unwrap().0);
        previous = current;
    }
    assert_eq!(changes, HashSet::from([NormalizedPath::new(path)]));
}

#[test]
fn deferred_digests_survive_until_they_can_be_compared() {
    let dir = tempdir().unwrap();
    let now = SystemTime::now();
    for name in ["a.cpp", "b.cpp"] {
        let path = dir.path().join(name);
        fs::write(&path, b"aaaa").unwrap();
        set_mtime(&path, now);
    }
    let mut previous = scan(dir.path());
    racy::verify(&HashMap::new(), &mut previous, now, 2).unwrap();
    let path = dir.path().join("b.cpp");
    fs::write(&path, b"bbbb").unwrap();
    set_mtime(&path, now);
    let mut current = scan(dir.path());
    assert!(
        racy::verify(&previous, &mut current, now + Duration::from_secs(3), 1)
            .unwrap()
            .0
            .is_empty()
    );
    let mut later = scan(dir.path());
    assert!(
        racy::verify(&current, &mut later, now + Duration::from_secs(4), 1)
            .unwrap()
            .0
            .contains(&NormalizedPath::new(&path))
    );
}

#[test]
fn unreadable_racy_content_fails_without_discarding_baseline() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("input.cpp");
    fs::write(&path, b"aaaa").unwrap();
    let now = SystemTime::now();
    let mut previous = scan(dir.path());
    racy::verify(
        &HashMap::new(),
        &mut previous,
        now - Duration::from_secs(1),
        1,
    )
    .unwrap();
    let mut current = scan(dir.path());
    fs::remove_file(&path).unwrap();
    assert!(racy::verify(&previous, &mut current, now, 1).is_err());
    assert!(previous.values().all(|state| state.racy.is_some()));
}

#[test]
fn perf_poll_old_files_has_zero_content_reads() {
    let dir = tempdir().unwrap();
    for index in 0..1000 {
        let path = dir.path().join(format!("input-{index}.cpp"));
        fs::write(&path, b"aaaa").unwrap();
        set_mtime(&path, UNIX_EPOCH + Duration::from_secs(1_000));
    }
    let config = build_config(dir.path(), &[], &["**/*.cpp".into()], &[]).unwrap();
    let (previous, _) = poll_snapshot(
        &config,
        DirectoryWalkParallelism::SharedPool,
        &HashMap::new(),
    )
    .unwrap();
    let started = Instant::now();
    let mut current = scan_snapshot(&config, DirectoryWalkParallelism::SharedPool).unwrap();
    let (changed, reads) = racy::verify(
        &previous,
        &mut current,
        SystemTime::now(),
        racy::HASHES_PER_POLL,
    )
    .unwrap();
    assert_eq!(
        reads, 0,
        "old files must keep the metadata-only path (#2169)"
    );
    assert!(changed.is_empty());
    assert!(diff_snapshots(&previous, &current).0.is_empty());
    // Hang/catastrophic-regression budget; the zero-read counter above proves
    // the structural fast-path property independently of runner speed.
    assert!(started.elapsed() < Duration::from_secs(10));
}
