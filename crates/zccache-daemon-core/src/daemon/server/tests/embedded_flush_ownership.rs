use super::{
    flush_step, join_task, stop_index_writer_task, EmbeddedDaemon, FlushStepOutcome,
    MaintenancePolicy,
};
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn ready_step_reports_completion() {
    let outcome = flush_step("test", async { Ok::<(), String>(()) }).await;
    assert_eq!(
        outcome.outcome,
        FlushStepOutcome::Completed,
        "a completed persistence step must be reported as complete"
    );
}

#[tokio::test]
async fn failed_step_preserves_error() {
    let outcome = flush_step("test", async { Err::<(), String>("disk full".into()) }).await;
    assert_eq!(
        outcome.outcome,
        FlushStepOutcome::Failed("disk full".into())
    );
}

#[tokio::test]
async fn task_shutdown_waits_for_owned_work() {
    let task = kernal_api::async_engine::launch(async {
        kernal_api::async_engine::sleep(Duration::from_millis(20)).await;
    });
    assert_eq!(
        join_task(task, "test task").await,
        FlushStepOutcome::Completed
    );
}

#[tokio::test]
async fn index_writer_shutdown_signal_survives_between_waits() {
    let shutdown = Arc::new(kernal_api::async_engine::Notify::new());
    let writer_shutdown = Arc::clone(&shutdown);
    let task = kernal_api::async_engine::launch(async move {
        kernal_api::async_engine::sleep(Duration::from_millis(20)).await;
        writer_shutdown.notified().await;
    });

    let step = tokio::time::timeout(
        Duration::from_secs(1),
        stop_index_writer_task(shutdown.as_ref(), Some(task)),
    )
    .await
    .expect("index writer shutdown signal was lost")
    .expect("writer handle produces a shutdown step");

    assert_eq!(step.outcome, FlushStepOutcome::Completed);
}

#[tokio::test]
async fn host_owned_disk_maintenance_still_owns_depgraph_scheduler() {
    let temp = tempfile::tempdir().expect("temp cache");
    let daemon = EmbeddedDaemon::start_with_maintenance(
        crate::ipc::unique_test_endpoint(),
        crate::core::NormalizedPath::new(temp.path()),
        None,
        None,
        MaintenancePolicy::default(),
        false,
        None,
    )
    .await
    .expect("embedded daemon");

    assert!(
        daemon
            .maintenance_handle
            .with_for(crate::core::sync::STATE_TIMEOUT, |handle| handle.is_none())
            .await,
        "host ownership must suppress only the disk scheduler"
    );
    assert!(
        daemon
            .depgraph_maintenance_handle
            .with_for(crate::core::sync::STATE_TIMEOUT, |handle| handle.is_some())
            .await,
        "depgraph persistence remains service-owned"
    );
    let report = daemon.shutdown().await;
    assert!(report.is_complete());
}

#[tokio::test]
async fn concurrent_embedded_daemons_keep_explicit_staging_roots_independent() {
    let cache_a = tempfile::tempdir().expect("cache a");
    let cache_b = tempfile::tempdir().expect("cache b");
    let staging_a = tempfile::tempdir().expect("staging a");
    let staging_b = tempfile::tempdir().expect("staging b");
    let cache_a = crate::core::NormalizedPath::new(cache_a.path());
    let cache_b = crate::core::NormalizedPath::new(cache_b.path());
    let staging_a = crate::core::NormalizedPath::new(staging_a.path());
    let staging_b = crate::core::NormalizedPath::new(staging_b.path());

    let start_a = EmbeddedDaemon::start_with_maintenance(
        crate::ipc::unique_test_endpoint(),
        cache_a,
        Some(&staging_a),
        None,
        MaintenancePolicy::default(),
        false,
        None,
    );
    let start_b = EmbeddedDaemon::start_with_maintenance(
        crate::ipc::unique_test_endpoint(),
        cache_b,
        Some(&staging_b),
        None,
        MaintenancePolicy::default(),
        false,
        None,
    );
    let (daemon_a, daemon_b) = tokio::join!(start_a, start_b);
    let daemon_a = daemon_a.expect("embedded daemon a");
    let daemon_b = daemon_b.expect("embedded daemon b");

    assert!(daemon_a
        .state
        .staging
        .path()
        .starts_with(staging_a.as_path()));
    assert!(daemon_b
        .state
        .staging
        .path()
        .starts_with(staging_b.as_path()));
    assert!(!daemon_a
        .state
        .staging
        .path()
        .starts_with(staging_b.as_path()));
    assert!(!daemon_b
        .state
        .staging
        .path()
        .starts_with(staging_a.as_path()));

    let (report_a, report_b) = tokio::join!(daemon_a.shutdown(), daemon_b.shutdown());
    assert!(report_a.is_complete());
    assert!(report_b.is_complete());
}
