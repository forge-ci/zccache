use super::*;

#[test]
fn long_wait_reports_once_without_terminating_legitimate_holder() {
    let now = Instant::now();
    let entry = Entry {
        id: 1,
        owner: 1,
        kind: "test",
        phase: Phase::Waiting,
        site: Location::caller(),
        thread: "test".into(),
        since: now - Duration::from_secs(30),
        limit: Duration::from_secs(15),
        reported: false,
    };
    let mut entries = vec![
        entry.clone(),
        Entry {
            id: 2,
            phase: Phase::Holding,
            limit: Duration::from_secs(120),
            ..entry
        },
    ];
    let (abort, snapshot) = decision(&mut entries, now).unwrap();
    assert!(!abort);
    let text = report(&snapshot, now, abort);
    assert!(text.contains("Waiting") && text.contains("Holding"));
    assert!(decision(&mut entries, now).is_none(), "wait reported twice");
    assert!(
        decision(&mut entries, now + Duration::from_secs(91))
            .unwrap()
            .0,
        "an exclusive copy must terminate after its longer hold budget"
    );
}

#[test]
fn native_guards_and_recovered_poison_unregister_on_drop() {
    let lock = crate::blocking_sync::Mutex::new(0);
    {
        let _held = lock.try_lock().unwrap();
        assert_eq!(registered(&lock), 1);
        assert!(lock.try_lock().is_err());
        assert_eq!(registered(&lock), 1, "failed try-lock leaked a waiter");
    }
    assert_eq!(registered(&lock), 0);
    let _ = std::panic::catch_unwind(|| {
        let _held = lock.lock_for(Duration::from_secs(1)).unwrap();
        panic!("poison the protected data");
    });
    assert_eq!(registered(&lock), 0);
    let poison = lock.lock_for(Duration::from_secs(1)).unwrap_err();
    assert_eq!(registered(&lock), 1, "poison still owns a native guard");
    drop(poison.into_inner());
    assert_eq!(registered(&lock), 0);
}

#[test]
fn poisoned_reader_and_writer_guards_remain_registered_until_release() {
    let lock = crate::blocking_sync::RwLock::new(0);
    let _ = std::panic::catch_unwind(|| {
        let _held = lock.write_for(Duration::from_secs(1)).unwrap();
        panic!("poison writer state");
    });
    assert_eq!(registered(&lock), 0);
    let reader = lock
        .read_for(Duration::from_secs(1))
        .unwrap_err()
        .into_inner();
    assert_eq!(registered(&lock), 1);
    drop(reader);
    assert_eq!(registered(&lock), 0);
    let writer = lock.try_write().unwrap_err();
    let std::sync::TryLockError::Poisoned(poison) = writer else {
        panic!("poison was hidden");
    };
    assert_eq!(registered(&lock), 1);
    drop(poison.into_inner());
    assert_eq!(registered(&lock), 0);
}

// Exercise the production decision, formatter and acknowledged writer with real
// registrations, independently of the monitor's best-effort abort grace.
fn report_expired_holder<T>(owner: &T) {
    let _waiter = Ticket::waiting(owner, "diagnostic waiter", Location::caller());
    let now = Instant::now() + crate::sync::STATE_TIMEOUT;
    let (abort, entries) = with_entries(|entries| decision(entries, now)).unwrap();
    assert!(abort, "an expired real holder must request termination");
    assert!(report_for(
        reporter(),
        report(&entries, now, abort),
        Duration::from_secs(5),
    ));
}

#[test]
fn holder_report_child() {
    let Ok(case) = std::env::var("ZCCACHE_WATCHDOG_TEST_CASE") else {
        return;
    };
    if case == "native" {
        let lock = crate::blocking_sync::Mutex::new(0);
        let _holder = lock.lock_for(Duration::from_secs(5)).unwrap();
        report_expired_holder(&lock);
        return;
    }
    let state = crate::sync::State::new(0);
    let operation = |_: &mut i32| report_expired_holder(&state);
    match case.as_str() {
        "blocking-state" => state.with_blocking_for(Duration::from_secs(5), operation),
        "async-state" => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(state.with_for(Duration::from_secs(5), operation)),
        _ => panic!("unexpected holder diagnostic child case"),
    }
}
