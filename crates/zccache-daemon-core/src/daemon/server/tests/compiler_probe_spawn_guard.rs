//! soldr#3836: a running compiler probe cannot own spawn admission.
#![allow(
    clippy::disallowed_methods,
    reason = "#2027: test fixtures spawn helper processes, not compilers"
)]

use super::{output_within, output_within_async, ProbeOutcome};
use std::time::{Duration, Instant};

fn check_probe_releases_admission(async_probe: bool) {
    let dir = tempfile::tempdir().unwrap();
    let ready = dir.path().join("ready");
    let release = dir.path().join("release");
    // The child is this test binary, not a shell. Windows PowerShell's cold
    // start on a loaded arm64 runner consumed most of the 20s probe budget
    // and timed the probe out (main runs 39915899065, 40145122511,
    // 40145828932, 40154740229, 40288447983); a native child starts in
    // milliseconds on every host and keeps the test about admission.
    let spec = kernal_api::SpawnSpec::new(std::env::current_exe().unwrap())
        .args(["--exact", PROBE_CHILD_FIXTURE, "--nocapture"])
        .env(PROBE_CHILD, "1")
        .env("ZCCACHE_TEST_PROBE_READY", ready.as_os_str())
        .env("ZCCACHE_TEST_PROBE_RELEASE", release.as_os_str());

    let probe = std::thread::spawn(move || {
        if async_probe {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(output_within_async(spec, Duration::from_secs(20)))
        } else {
            output_within(spec, Duration::from_secs(20))
        }
    });
    let started = Instant::now();
    while !ready.exists() && started.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(10));
    }
    // Child readiness proves we are testing an in-flight wait, rather than
    // racing the probe's startup. Release on every assertion path.
    if !ready.exists() {
        let _ = std::fs::write(&release, b"release");
        let _ = probe.join();
        panic!("compiler probe never signalled readiness");
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let _guard = crate::daemon::spawn_exclusion::materialize_exclusive();
        let _ = tx.send(());
    });
    let acquired_while_probe_waited = rx.recv_timeout(Duration::from_secs(2)).is_ok();
    std::fs::write(&release, b"release").unwrap();
    let outcome = probe.join().unwrap();
    writer.join().unwrap();
    assert!(
        acquired_while_probe_waited,
        "probe held spawn admission while waiting for the child"
    );
    match outcome {
        ProbeOutcome::Completed(output) if output.success => {}
        ProbeOutcome::Completed(output) => panic!(
            "probe child failed: stdout={}",
            String::from_utf8_lossy(&output.stdout)
        ),
        ProbeOutcome::TimedOut => panic!("probe timed out after {:?}", started.elapsed()),
        ProbeOutcome::SpawnFailed => panic!("probe child failed to spawn"),
    }
}

const PROBE_CHILD: &str = "ZCCACHE_TEST_PROBE_CHILD";
const PROBE_CHILD_FIXTURE: &str =
    "daemon::server::compiler_hash::probe_spawn_guard_tests::probe_child_fixture";

/// The in-flight child for the probe tests: signal readiness, then wait
/// (bounded) for the parent's release file. A no-op in a normal test run.
#[test]
fn probe_child_fixture() {
    if std::env::var_os(PROBE_CHILD).is_none() {
        return;
    }
    let ready = std::env::var_os("ZCCACHE_TEST_PROBE_READY").unwrap();
    let release = std::path::PathBuf::from(std::env::var_os("ZCCACHE_TEST_PROBE_RELEASE").unwrap());
    std::fs::write(ready, b"ready").unwrap();
    let started = Instant::now();
    while !release.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "probe parent never released the child"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn sync_probe_does_not_hold_spawn_admission_while_waiting() {
    check_probe_releases_admission(false);
}

#[test]
fn async_probe_does_not_hold_spawn_admission_while_waiting() {
    check_probe_releases_admission(true);
}
