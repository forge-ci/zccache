//! #2047: a poll whose walk ends early on a saturated shared rayon pool must
//! surface as overflow, never as changes to files that did not change. One
//! test in its own binary: saturating the process-wide pool would disturb any
//! test sharing this process.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};
use zccache_core::NormalizedPath;
use zccache_watcher::{PollWatchBatch, PollingWatcher, PollingWatcherConfig};

const SHARED_POOL_HOLD: Duration = Duration::from_millis(2500);
const SATURATION_ATTEMPTS: usize = 3;

fn saturate_shared_pool() {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let workers = rayon::current_num_threads();
    for _ in 0..workers {
        let started_tx = started_tx.clone();
        rayon::spawn(move || {
            started_tx.send(()).unwrap();
            std::thread::sleep(SHARED_POOL_HOLD);
        });
    }
    for _ in 0..workers {
        started_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("rayon worker should start");
    }
}

fn drain_for(watcher: &PollingWatcher, window: Duration) -> Vec<PollWatchBatch> {
    let deadline = Instant::now() + window;
    let mut batches = Vec::new();
    while Instant::now() < deadline {
        if let Some(batch) = watcher.poll_timeout(Duration::from_millis(50)).unwrap() {
            batches.push(batch);
        }
    }
    batches
}

#[test]
fn saturated_pool_reports_overflow_and_keeps_the_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("watch.cpp");
    std::fs::write(&file, "a\n").unwrap();

    let mut config = PollingWatcherConfig::new(dir.path());
    config.poll_interval = Duration::from_millis(20);
    config.debounce = Duration::from_millis(20);
    let watcher = PollingWatcher::new(config).unwrap();
    watcher.start().unwrap();

    // A scan already past its walk blocks out one saturation window without
    // failing, so allow a few windows for a scan to hit the busy pool.
    let mut batches = Vec::new();
    for _ in 0..SATURATION_ATTEMPTS {
        saturate_shared_pool();
        batches.extend(drain_for(&watcher, SHARED_POOL_HOLD * 2));
        if batches.iter().any(|batch| batch.overflow) {
            break;
        }
    }

    assert!(
        batches.iter().any(|batch| batch.overflow),
        "a failed walk must be reported: {batches:?}"
    );
    assert!(
        batches
            .iter()
            .all(|batch| batch.changed.is_empty() && batch.removed.is_empty()),
        "an unchanged file must not be reported: {batches:?}"
    );

    std::fs::write(&file, "bb\n").unwrap();
    let batch = drain_for(&watcher, Duration::from_secs(5))
        .into_iter()
        .find(|batch| !batch.changed.is_empty())
        .expect("a change after recovery must be detected");
    watcher.stop().unwrap();
    assert_eq!(
        batch.changed,
        vec![NormalizedPath::new(file.canonicalize().unwrap())]
    );

    // A baseline taken on the saturated pool must still see the whole tree.
    saturate_shared_pool();
    watcher.resume().unwrap();
    let batches = drain_for(&watcher, SHARED_POOL_HOLD * 2);
    watcher.stop().unwrap();
    assert!(
        batches.iter().all(|batch| batch.changed.is_empty()),
        "a complete baseline reports no changes: {batches:?}"
    );
}
