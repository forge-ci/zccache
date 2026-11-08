//! #2047: `snapshot_fp::record` must inventory the whole workspace while the
//! shared rayon pool is saturated. One test in its own binary: saturating the
//! process-wide pool would disturb any test sharing this process.
#![cfg(feature = "cli")]

use std::time::Duration;
use zccache_cli_core::cli::snapshot_fp::{record, Manifest, MANIFEST_FILENAME};

const SHARED_POOL_HOLD: Duration = Duration::from_millis(2500);

// Worker startup failures are fatal to this test's saturation precondition.
#[allow(clippy::unwrap_used, clippy::expect_used)]
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

#[test]
fn record_covers_build_script_crate_with_saturated_shared_pool() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let target = workspace.join("target");
    let fp_dir = target.join("debug/.fingerprint/baz-0123456789abcdef");
    std::fs::create_dir_all(&fp_dir).unwrap();
    std::fs::create_dir_all(workspace.join("crates/baz")).unwrap();
    std::fs::write(workspace.join("crates/baz/build.rs"), "fn main() {}\n").unwrap();
    std::fs::write(
        workspace.join("crates/baz/Cargo.toml"),
        "[package]\nname = \"baz\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let fingerprint = serde_json::json!({
        "local": [{
            "RerunIfChanged": {
                "output": "debug/build/baz-0123456789abcdef/output",
                "paths": ["build.rs"],
            }
        }]
    });
    std::fs::write(
        fp_dir.join("run-build-script-build-script-build.json"),
        fingerprint.to_string(),
    )
    .unwrap();
    let manifest_path = target.join(MANIFEST_FILENAME);

    saturate_shared_pool();
    let stats = record(&target, &workspace, &manifest_path, "debug").unwrap();

    assert_eq!(stats.crates_recorded, 1, "partial inventory: {stats:?}");
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(manifest.crates[0].fingerprint_dir, "baz-0123456789abcdef");
}
