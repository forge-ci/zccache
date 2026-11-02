//! Deterministic coverage for the exact-exec coalescing races from #971.

use super::{
    acquire_in_flight, coalesce_wait, store_exec_artifact, try_coalesced_exec_hit,
    try_exec_cache_hit, ArtifactData, CoalesceOutcome, ExecOutputStreams, InFlight,
    MaterializationMode, Response,
};
use dashmap::DashMap;
use kernal_api::async_engine::Notify;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "current_thread")]
async fn stale_miss_rechecks_peer_result_after_becoming_new_owner() {
    let directory = tempfile::tempdir().unwrap();
    let server = crate::daemon::server::tests::bind_isolated_server(directory.path());
    let state = &server.state;
    let key = "late-owner-after-peer-published";
    let streams = ExecOutputStreams::default();
    let mode = MaterializationMode::Copy;
    assert!(
        try_exec_cache_hit(state, key, directory.path(), &[], streams, mode)
            .await
            .is_none()
    );

    // The peer finishes after this caller's lookup missed, but before the
    // caller resumes to claim its slot. No scheduler timing is required.
    let peer = acquire_in_flight(state, key).await;
    assert!(matches!(peer.outcome, InFlight::Owner));
    store_exec_artifact(
        state,
        key.to_owned(),
        ArtifactData {
            outputs: Vec::new(),
            stdout: Arc::new(b"published by peer".to_vec()),
            stderr: Arc::new(Vec::new()),
            exit_code: 0,
        },
        None,
    )
    .await
    .unwrap();
    drop(peer);
    assert!(!state.in_flight_exec.contains_key(key));

    let late_owner = acquire_in_flight(state, key).await;
    assert!(matches!(late_owner.outcome, InFlight::Owner));
    let response = try_coalesced_exec_hit(&late_owner, directory.path(), &[], streams, mode).await;
    assert!(
        matches!(response, Some(Response::GenericToolExecResult { cached: true, ref stdout, .. }) if stdout.as_slice() == b"published by peer"),
        "a stale MISS must not spawn again after a peer already published"
    );
}

async fn guarded(future: impl std::future::Future<Output = CoalesceOutcome>) -> CoalesceOutcome {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .expect("coalesce_wait hung — the #971 fix regressed")
}

#[tokio::test]
async fn slot_gone_resolves_without_waiting() {
    let map = DashMap::new();
    let ours = Arc::new(Notify::new());
    let outcome = guarded(coalesce_wait(&map, "k", ours, Duration::from_secs(30))).await;
    assert_eq!(outcome, CoalesceOutcome::SlotResolved);
}

#[tokio::test]
async fn slot_replaced_resolves_without_waiting() {
    let map = DashMap::new();
    let ours = Arc::new(Notify::new());
    map.insert("k".to_string(), Arc::new(Notify::new()));
    let outcome = guarded(coalesce_wait(&map, "k", ours, Duration::from_secs(30))).await;
    assert_eq!(outcome, CoalesceOutcome::SlotResolved);
}

#[tokio::test]
async fn owner_notification_wakes_waiter() {
    let map = DashMap::new();
    let shared = Arc::new(Notify::new());
    map.insert("k".to_string(), Arc::clone(&shared));
    let waker = {
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            shared.notify_waiters();
        })
    };
    let outcome = guarded(coalesce_wait(&map, "k", shared, Duration::from_secs(30))).await;
    assert_eq!(outcome, CoalesceOutcome::Woken);
    waker.await.unwrap();
}

#[tokio::test]
async fn wedged_owner_times_out() {
    let map = DashMap::new();
    let shared = Arc::new(Notify::new());
    map.insert("k".to_string(), Arc::clone(&shared));
    let outcome = guarded(coalesce_wait(&map, "k", shared, Duration::from_millis(50))).await;
    assert_eq!(outcome, CoalesceOutcome::TimedOut);
}
