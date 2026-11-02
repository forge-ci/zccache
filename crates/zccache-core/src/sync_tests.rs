use super::*;

#[tokio::test]
async fn state_access_finishes_before_the_next_await() {
    let state = State::new(Vec::new());
    state.with_for(STATE_TIMEOUT, |values| values.push(1)).await;
    let snapshot = state.with_for(STATE_TIMEOUT, |values| values.clone()).await;
    state.with_for(STATE_TIMEOUT, |values| values.push(2)).await;
    assert_eq!(snapshot, [1]);
}

#[tokio::test]
async fn cancelled_gate_wait_does_not_keep_its_queue_place() {
    let gate = Arc::new(Gate::new());
    let held = gate.acquire_for(STATE_TIMEOUT).await;
    assert!(gate
        .try_acquire_for(Duration::from_millis(1))
        .await
        .is_err());
    drop(held);
    assert!(gate.try_acquire_for(Duration::from_secs(1)).await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_contention_returns_a_timeout_without_running_the_callback() {
    let state = Arc::new(State::new(0));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker_state = Arc::clone(&state);
    let worker = tokio::task::spawn_blocking(move || {
        worker_state.with_blocking_for(STATE_TIMEOUT, |value| {
            let _ = entered_tx.send(());
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            *value = 1;
        });
    });
    entered_rx.await.unwrap();
    let result = state
        .try_with_for(Duration::from_millis(1), |_| {
            panic!("timed out callback ran")
        })
        .await;
    release_tx.send(()).unwrap();
    worker.await.unwrap();
    assert!(result.is_err());
    assert_eq!(state.with_for(STATE_TIMEOUT, |value| *value).await, 1);
}

#[tokio::test]
async fn exclusive_barrier_wait_is_bounded_and_cancellation_releases_the_queue() {
    let barrier = Barrier::new();
    let reader = barrier.read_for(STATE_TIMEOUT).await;
    assert!(barrier
        .try_write_for(Duration::from_millis(1))
        .await
        .is_err());
    drop(reader);
    assert!(barrier.try_write_for(Duration::from_secs(1)).await.is_ok());
}

#[test]
fn blocking_lock_keeps_poisoning_visible() {
    let mutex = super::super::blocking_sync::Mutex::new(0);
    let result = std::panic::catch_unwind(|| {
        let mut guard = mutex.lock_for(STATE_TIMEOUT).unwrap();
        *guard = 1;
        panic!("incomplete cache mutation");
    });
    assert!(result.is_err());
    assert!(mutex.lock_for(STATE_TIMEOUT).is_err());
}

#[test]
fn runtime_bridge_bounds_a_pending_future() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert!(super::super::wait::block_on_for(
        &runtime,
        std::future::pending::<()>(),
        Duration::from_millis(1)
    )
    .is_err());
}

#[test]
fn bounded_send_returns_unsent_value_on_timeout_and_disconnect() {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    super::super::wait::send_for(&tx, 1, Duration::ZERO).unwrap();
    assert!(matches!(
        super::super::wait::send_for(&tx, 2, Duration::ZERO),
        Err(std::sync::mpsc::TrySendError::Full(2))
    ));
    drop(rx);
    assert!(matches!(
        super::super::wait::send_for(&tx, 3, Duration::ZERO),
        Err(std::sync::mpsc::TrySendError::Disconnected(3))
    ));
}

#[test]
fn blocking_barrier_writer_queues_before_later_readers() {
    let barrier = Barrier::new();
    let first = barrier.blocking_read_for(Duration::from_secs(5));
    let writer_barrier = barrier.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        tx.send(()).unwrap();
        writer_barrier.blocking_write_for(Duration::from_secs(5))
    });
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let started = std::time::Instant::now();
    let queued = loop {
        if barrier.try_read_owned().is_err() {
            break true;
        }
        if started.elapsed() >= Duration::from_secs(1) {
            break false;
        }
        std::thread::yield_now();
    };
    // Release and join on the failure path too: the test must not strand the
    // writer or turn a fairness assertion into a timeout abort.
    drop(first);
    drop(writer.join().unwrap());
    assert!(
        queued,
        "blocking writer never queued ahead of later readers"
    );
}

#[tokio::test]
async fn gate_permit_keeps_weak_pool_owner_alive() {
    let gate = Arc::new(Gate::new());
    let weak = Arc::downgrade(&gate);
    let permit = gate.acquire_for(STATE_TIMEOUT).await;
    drop(gate);
    let owner = weak.upgrade();
    drop(permit);
    assert!(
        owner.is_some(),
        "a live permit must retain its weak-pool gate identity"
    );
    drop(owner);
    assert!(
        weak.upgrade().is_none(),
        "an idle gate must still be reclaimable"
    );
}
