//! Real subprocess regressions: a wedged callback cannot drive its own timer.
use super::*;

fn assert_holder_is_terminated(case: &str) {
    let spec = kernal_api::SpawnSpec::new(std::env::current_exe().unwrap())
        .args(["--exact", "sync::watchdog_tests::watchdog_child"])
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
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sync::watchdog_tests::watchdog_child"])
        .env("ZCCACHE_WATCHDOG_TEST_CASE", case)
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
fn essential_acquisition_expiry_dumps_holder_and_waiter_despite_capture() {
    let output = crate::wait::output_for(
        kernal_api::SpawnSpec::new(std::env::current_exe().unwrap())
            .args(["--exact", "sync::watchdog_tests::watchdog_child"])
            .env("ZCCACHE_WATCHDOG_TEST_CASE", "acquisition"),
        Duration::from_secs(5),
    )
    .unwrap();
    assert_ne!(output.exit.raw_code(), 0);
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
    if case == "acquisition" {
        let lock = crate::blocking_sync::Mutex::with_hold_limit(0, Duration::from_secs(60));
        let _holder = lock.lock_for(Duration::from_secs(1)).unwrap();
        let _waiter = lock.lock_for(Duration::from_millis(1)).unwrap();
        unreachable!("the second acquisition cannot complete");
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
