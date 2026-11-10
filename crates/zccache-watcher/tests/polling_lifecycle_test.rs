#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{fs, time::Duration};
use zccache_core::NormalizedPath;
use zccache_watcher::{PollingWatcher, PollingWatcherConfig};

/// Rust public-API counterpart of Python's context-manager/resume test (#2169).
#[test]
fn immediate_size_change_reports_path_before_and_after_resume() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("watch.cpp");
    fs::write(&path, b"a\n").unwrap();
    let expected = NormalizedPath::new(path.canonicalize().unwrap());
    let mut config = PollingWatcherConfig::new(dir.path());
    config.include_globs = vec!["**/*.cpp".into()];
    config.poll_interval = Duration::from_millis(50);
    config.debounce = Duration::from_millis(50);
    let watcher = PollingWatcher::new(config).unwrap();
    watcher.start().unwrap();
    fs::write(&path, b"bb\n").unwrap();
    let batch = watcher
        .poll_timeout(Duration::from_secs(1))
        .unwrap()
        .expect("first edit batch");
    assert!(!batch.overflow);
    assert_eq!(batch.changed.as_slice(), std::slice::from_ref(&expected));
    assert!(batch.removed.is_empty());
    watcher.stop().unwrap();

    fs::write(&path, b"c\n").unwrap();
    assert!(watcher
        .poll_timeout(Duration::from_millis(200))
        .unwrap()
        .is_none());
    watcher.resume().unwrap();
    assert!(watcher
        .poll_timeout(Duration::from_millis(200))
        .unwrap()
        .is_none());
    fs::write(&path, b"d\n").unwrap();
    let batch = watcher
        .poll_timeout(Duration::from_secs(1))
        .unwrap()
        .expect("resumed edit batch");
    assert!(!batch.overflow);
    assert_eq!(batch.changed.as_slice(), std::slice::from_ref(&expected));
    assert!(batch.removed.is_empty());
    watcher.stop().unwrap();
}
