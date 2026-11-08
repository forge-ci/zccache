//! The shared test env lock wait is bounded and keeps recovering poison (#2098).
use super::lock_env_within;
use std::sync::Mutex;
use std::time::Duration;

#[test]
fn held_lock_panics_at_the_deadline_naming_the_wait() {
    let lock = std::sync::Arc::new(Mutex::new(()));
    let held = lock.lock().unwrap();
    let waiter_lock = std::sync::Arc::clone(&lock);
    let waiter = std::thread::spawn(move || {
        let _never = lock_env_within(&waiter_lock, Duration::from_millis(5));
    });
    let result = zccache_core::wait::join_for(waiter, Duration::from_secs(5));
    drop(held);
    let result = match result {
        Ok(result) => result,
        Err(waiter) => {
            // Release the holder before cleanup so even a broken acquire can exit.
            let _ = zccache_core::wait::join_for(waiter, Duration::from_secs(5))
                .expect("env lock waiter must stop after releasing the holder");
            panic!("env lock waiter did not enforce its deadline");
        }
    };
    let panic = result.expect_err("a held env lock must time out");
    let message = panic.downcast_ref::<String>().unwrap();
    assert!(
        message.contains("waiting for the test env lock"),
        "{message}"
    );
}

#[test]
fn free_lock_is_acquired() {
    let lock = Mutex::new(());
    let _guard = lock_env_within(&lock, Duration::from_millis(5));
}

#[test]
fn poisoned_lock_is_recovered() {
    let lock = Mutex::new(());
    std::thread::scope(|scope| {
        let holder = scope.spawn(|| {
            let _held = lock.lock().unwrap();
            panic!("poison the lock");
        });
        assert!(holder.join().is_err());
    });
    assert!(lock.is_poisoned());
    let _guard = lock_env_within(&lock, Duration::from_millis(5));
}
