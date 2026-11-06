//! The shared test env lock wait is bounded and keeps recovering poison (#2098).
use super::lock_env_within;
use std::sync::Mutex;
use std::time::Duration;

#[test]
#[should_panic(expected = "waiting for the test env lock")]
fn held_lock_panics_at_the_deadline_naming_the_wait() {
    let lock = Mutex::new(());
    let _held = lock.lock().unwrap();
    let _never = lock_env_within(&lock, Duration::from_millis(5));
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
