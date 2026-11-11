//! Real subprocess regressions: a wedged callback cannot drive its own timer.
use super::*;

fn assert_holder_is_terminated(case: &str) {
    let cache = tempfile::tempdir().unwrap();
    let spec = kernal_api::SpawnSpec::new(std::env::current_exe().unwrap())
        .args(["--exact", "sync::watchdog_tests::watchdog_child"])
        .env("ZCCACHE_CACHE_DIR", cache.path())
        .env("ZCCACHE_DAEMON_STATE_DIR", cache.path())
        .env("ZCCACHE_WATCHDOG_TEST_CASE", case);
    // Keep libtest capture enabled in the child: diagnostics must bypass it.
    let output = crate::wait::output_for(spec, STATE_TIMEOUT + Duration::from_secs(5))
        .expect("a wedged holder must terminate before the parent deadline");
    assert_ne!(
        output.exit.raw_code(),
        0,
        "wedged holder exited successfully"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    for expected in [
        "lock hold exceeded",
        "Holding",
        "Waiting",
        "sync_watchdog_tests.rs",
    ] {
        assert!(stderr.contains(expected), "missing {expected:?}: {stderr}");
    }
    let crashes = cache.path().join("crashes");
    let dump = std::fs::read_dir(crashes)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with("watchdog.txt")
        })
        .expect("watchdog reason must survive process termination on disk");
    let report = std::fs::read_to_string(dump.path()).unwrap();
    for expected in [
        "lock hold exceeded",
        "Holding",
        "Waiting",
        "sync_watchdog_tests.rs",
    ] {
        assert!(report.contains(expected), "missing {expected:?}: {report}");
    }
}

#[test]
fn watchdog_terminates_blocked_state_callback() {
    assert_holder_is_terminated("blocking-state");
}

#[test]
fn watchdog_terminates_blocked_async_callback() {
    assert_holder_is_terminated("async-state");
}

#[test]
fn watchdog_terminates_native_guard_holder() {
    assert_holder_is_terminated("native");
}

fn assert_termination_with_unread_stderr(case: &str) {
    let cache = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sync::watchdog_tests::watchdog_child"])
        .env("ZCCACHE_WATCHDOG_TEST_CASE", case)
        .env("ZCCACHE_CACHE_DIR", cache.path())
        .env("ZCCACHE_DAEMON_STATE_DIR", cache.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Deliberately own but never drain the pipe: the child fills it while
    // holding stderr's internal lock. This test must not use captured output.
    let _unread_stderr = child.stderr.take().unwrap();
    let status = crate::wait::child_for(&mut child, STATE_TIMEOUT + Duration::from_secs(5))
        .expect("a blocked diagnostic sink must not prevent termination");
    assert!(!status.success());
    // #2064: unread stderr must not swallow the fatal reason, either.
    let dumps = std::fs::read_dir(cache.path().join("crashes")).unwrap();
    let report = dumps
        .filter_map(Result::ok)
        .find_map(|entry| {
            let body = std::fs::read_to_string(entry.path()).ok()?;
            body.contains("sync_watchdog_tests.rs").then_some(body)
        })
        .expect("fatal reason must reach disk before the blocked stderr write");
    assert!(report.contains("Holding"));
    if case == "blocked-acquisition-stderr" {
        assert!(report.contains("Waiting"), "{report}");
        assert!(
            report.contains("bounded synchronization failed"),
            "{report}"
        );
    }
    assert!(
        report.contains("bounded synchronization failed") || report.contains("lock hold exceeded")
    );
}

#[test]
fn blocked_stderr_cannot_disable_holder_termination() {
    assert_termination_with_unread_stderr("blocked-stderr");
}

#[test]
fn blocked_stderr_cannot_disable_essential_acquisition_termination() {
    assert_termination_with_unread_stderr("blocked-acquisition-stderr");
}

fn block_stderr() {
    use std::io::Write;
    let (ready, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stderr = std::io::stderr().lock();
        ready.send(()).unwrap();
        stderr.write_all(&vec![b'x'; 16 * 1024 * 1024]).unwrap();
        park_forever();
    });
    receiver.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn essential_acquisition_report_dumps_holder_and_waiter_despite_capture() {
    let cache = tempfile::tempdir().unwrap();
    let output = crate::wait::output_for(
        kernal_api::SpawnSpec::new(std::env::current_exe().unwrap())
            .args(["--exact", "sync::watchdog_tests::watchdog_child"])
            .env("ZCCACHE_CACHE_DIR", cache.path())
            .env("ZCCACHE_DAEMON_STATE_DIR", cache.path())
            .env("ZCCACHE_WATCHDOG_TEST_CASE", "acquisition-report"),
        Duration::from_secs(10),
    )
    .unwrap();
    assert_eq!(
        output.exit.raw_code(),
        0,
        "diagnostic child failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    for expected in [
        "bounded synchronization failed",
        "Holding",
        "Waiting",
        "sync_watchdog_tests.rs",
    ] {
        assert!(stderr.contains(expected), "missing {expected}: {stderr}");
    }
}

#[tokio::test]
async fn cancelled_state_waiters_unregister_without_releasing_another_owner() {
    use std::future::Future;
    let state = State::new(0);
    let guard = state.inner.lock().await;
    {
        let mut waiter = std::pin::pin!(state.try_with_for(Duration::from_secs(1), |_| ()));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(waiter.as_mut().poll(&mut context).is_pending());
        assert_eq!(crate::sync_watchdog::registered(&state), 1);
    }
    assert_eq!(crate::sync_watchdog::registered(&state), 0);
    assert!(state
        .try_with_for(Duration::from_millis(1), |_| ())
        .await
        .is_err());
    assert_eq!(crate::sync_watchdog::registered(&state), 0);
    drop(guard);
    state.with_for(Duration::from_secs(1), |_| ()).await;
    assert_eq!(crate::sync_watchdog::registered(&state), 0);
}

#[test]
fn long_operation_permit_is_not_a_short_data_hold() {
    let output = crate::wait::output_for(
        kernal_api::SpawnSpec::new(std::env::current_exe().unwrap())
            .args(["--exact", "sync::watchdog_tests::watchdog_child"])
            .env("ZCCACHE_WATCHDOG_TEST_CASE", "operation"),
        STATE_TIMEOUT + Duration::from_secs(5),
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

#[test]
fn watchdog_child() {
    let Ok(case) = std::env::var("ZCCACHE_WATCHDOG_TEST_CASE") else {
        return;
    };
    if case == "blocked-stderr" {
        let lock = crate::blocking_sync::Mutex::new(0);
        let _holder = lock.lock_for(Duration::from_secs(1)).unwrap();
        block_stderr();
        park_forever();
    }
    if case == "blocked-acquisition-stderr" {
        let lock = crate::blocking_sync::Mutex::with_hold_limit(0, Duration::from_secs(60));
        let _holder = lock.lock_for(Duration::from_secs(1)).unwrap();
        block_stderr();
        let _waiter = lock.lock_for(Duration::from_millis(1)).unwrap();
        unreachable!("a held mutex cannot be acquired twice");
    }
    if case == "acquisition-report" {
        let lock = crate::blocking_sync::Mutex::with_hold_limit(0, Duration::from_secs(60));
        let _holder = lock.lock_for(Duration::from_secs(1)).unwrap();
        let site = std::panic::Location::caller();
        let _waiter = crate::sync_watchdog::Ticket::waiting(&lock, "Mutex", site);
        // Fatal delivery is best effort within 250 ms, including crash-dump
        // fsync. Test capture independently by acknowledging the actual write;
        // blocked-acquisition-stderr separately proves bounded termination.
        assert!(crate::sync_watchdog::report_acquisition_for(
            site,
            LockTimeout(Duration::from_millis(1)),
            Duration::from_secs(5),
        ));
        return;
    }
    if case == "operation" {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let gate = Arc::new(Gate::new());
        let _permit = runtime.block_on(gate.acquire_for(Duration::from_secs(1)));
        std::thread::sleep(STATE_TIMEOUT + Duration::from_millis(250));
        return;
    }
    if case == "native" {
        let lock = Arc::new(crate::blocking_sync::Mutex::new(0));
        let _holder = lock.lock_for(Duration::from_secs(60)).unwrap();
        let waiter = Arc::clone(&lock);
        std::thread::spawn(move || {
            let _waiter = waiter.lock_for(Duration::from_secs(60)).unwrap();
            park_forever();
        });
        park_forever();
    }
    let state = Arc::new(State::new(0));
    let waiter = Arc::clone(&state);
    let operation = move |_: &mut i32| {
        std::thread::spawn(move || {
            waiter.with_blocking_for(Duration::from_secs(60), |_| ());
        });
        park_forever();
    };
    match case.as_str() {
        "blocking-state" => state.with_blocking_for(Duration::from_secs(60), operation),
        "async-state" => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(state.with_for(Duration::from_secs(60), operation)),
        _ => panic!("unexpected watchdog child case"),
    }
}
