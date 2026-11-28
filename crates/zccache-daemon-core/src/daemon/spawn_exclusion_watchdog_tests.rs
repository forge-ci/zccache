#![allow(
    clippy::disallowed_methods,
    reason = "#2027: test fixtures spawn helper processes, not compilers"
)]
use super::*;
use std::time::Duration;

fn assert_reports_without_abort(case: &str, budget: Duration, kind: &str) {
    let output = zccache_core::wait::output_for(
        kernal_api::SpawnSpec::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon::spawn_exclusion::watchdog_tests::watchdog_child",
            ])
            .env("ZCCACHE_SPAWN_WATCHDOG_TEST_CASE", case),
        budget + Duration::from_secs(20),
    )
    .expect("an overdue spawn/materialization holder must be allowed to finish");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Overrunning the hold budget is reported, never fatal: the holder may be
    // blocked in a stalled filesystem syscall that an abort cannot cancel.
    assert_eq!(output.exit.raw_code(), 0, "{stderr}");
    assert!(stderr.contains("lock hold exceeded"), "{stderr}");
    let holding = stderr
        .lines()
        .find(|line| line.contains(&format!("Holding {kind}")))
        .unwrap_or_else(|| panic!("missing holder: {stderr}"));
    assert!(
        holding.contains("spawn_exclusion_watchdog_tests.rs"),
        "{holding}"
    );
    assert!(
        stderr
            .lines()
            .any(|line| line.contains("Waiting")
                && line.contains("spawn_exclusion_watchdog_tests.rs")),
        "{stderr}"
    );
}

#[test]
fn watchdog_reports_slow_spawn_holder_without_abort() {
    assert_reports_without_abort("spawn", zccache_core::sync::STATE_TIMEOUT, "RwLock(read)");
}

#[test]
fn watchdog_reports_slow_materialization_holder_without_abort() {
    // Exercise the actual production copy budget, without a test-only override.
    assert_reports_without_abort(
        "materialize",
        zccache_core::sync::STATE_TIMEOUT * 8,
        "RwLock(write)",
    );
}

#[test]
fn legitimate_copy_can_outlive_short_budget_with_real_admission_waiters() {
    let output = zccache_core::wait::output_for(
        kernal_api::SpawnSpec::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon::spawn_exclusion::watchdog_tests::watchdog_child",
            ])
            .env("ZCCACHE_SPAWN_WATCHDOG_TEST_CASE", "legitimate-copy"),
        zccache_core::sync::STATE_TIMEOUT + Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(
        output.exit.raw_code(),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn park_forever() -> ! {
    loop {
        std::thread::park();
    }
}

/// Hold past `budget` (plus the monitor period and report delivery), release
/// and exit cleanly. The watchdog must have reported, not aborted, by then.
fn overrun_then_release<G>(holder: G, budget: Duration) {
    std::thread::sleep(budget + Duration::from_secs(2));
    drop(holder);
    // Let the waiter take the lock and register before the process exits.
    std::thread::sleep(Duration::from_millis(200));
    std::process::exit(0);
}

#[test]
fn watchdog_child() {
    let Ok(case) = std::env::var("ZCCACHE_SPAWN_WATCHDOG_TEST_CASE") else {
        return;
    };
    match case.as_str() {
        "legitimate-copy" => {
            let holder = materialize_exclusive();
            let spawn = std::thread::spawn(|| {
                let _permit = spawn_shared();
            });
            let copy = std::thread::spawn(|| {
                let _permit = materialize_exclusive();
            });
            std::thread::sleep(zccache_core::sync::STATE_TIMEOUT + Duration::from_millis(500));
            drop(holder);
            zccache_core::wait::join_for(spawn, Duration::from_secs(2))
                .unwrap()
                .unwrap();
            zccache_core::wait::join_for(copy, Duration::from_secs(2))
                .unwrap()
                .unwrap();
        }
        "spawn" => {
            let holder = spawn_shared();
            std::thread::spawn(|| {
                let _waiter = SPAWN_MATERIALIZE_LOCK
                    .write_for(zccache_core::sync::OPERATION_TIMEOUT)
                    .unwrap();
                park_forever();
            });
            overrun_then_release(holder, zccache_core::sync::STATE_TIMEOUT);
        }
        "materialize" => {
            let holder = materialize_exclusive();
            std::thread::spawn(|| {
                let _waiter = SPAWN_MATERIALIZE_LOCK
                    .read_for(zccache_core::sync::OPERATION_TIMEOUT)
                    .unwrap();
                park_forever();
            });
            overrun_then_release(holder, zccache_core::sync::STATE_TIMEOUT * 8);
        }
        _ => panic!("unknown spawn watchdog case"),
    }
}
