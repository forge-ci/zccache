//! Cache-record destructors must not recursively clean private plans under publication.
use super::*;

#[tokio::test]
async fn clear_releases_publication_before_last_provisional_owner_cleanup() {
    crate::test_support::test_timeout(async {
        let directory = tempfile::tempdir().unwrap();
        let cache_dir = NormalizedPath::new(directory.path());
        let endpoint = crate::ipc::unique_test_endpoint();
        let mut server = DaemonServer::bind_with_cache_dir(&endpoint, &cache_dir).unwrap();
        let index_writer = tokio::spawn(run_index_writer(
            server.index_writer_rx.take().unwrap(),
            Arc::clone(&server.state.artifact_store),
            Arc::clone(&server.state.index_writer_shutdown),
            server
                .state
                .cache_root_lock
                .lease_for(STATE_TIMEOUT)
                .unwrap(),
        ));
        let private = cache_dir.join("private-plan");
        std::fs::create_dir_all(&private).unwrap();
        let output = private.join("output.rlib");
        std::fs::write(&output, b"private").unwrap();
        let owner = Arc::new(StagedCompilePlan::for_test(private.clone(), Vec::new()));
        let key = "d".repeat(64);
        server.state.artifacts.insert(
            key.clone(),
            CachedArtifact::from_provisional_staged(
                ArtifactIndex::new(
                    vec!["output.rlib".into()],
                    vec![7],
                    Arc::new(Vec::new()),
                    Arc::new(Vec::new()),
                    0,
                ),
                vec![output],
                owner,
            ),
        );
        let hook = StagedHookGuard::arm(&private, StagedHookPoint::PrivatePlanCleanup);
        let state = Arc::clone(&server.state);
        let clear =
            tokio::spawn(
                async move { super::super::super::handle_clear::handle_clear(&state).await },
            );
        let hook = tokio::task::spawn_blocking(move || {
            hook.wait_until_reached_for(std::time::Duration::from_secs(10));
            hook
        })
        .await
        .unwrap();
        let read = server.state.artifact_publication.try_read_owned();
        let publication_permitted = read.is_ok();
        let old_entry_unreachable =
            publication_permitted && !server.state.artifacts.contains_key(&key);
        let new_key = crate::artifact::Key::from_hex(&"e".repeat(64)).unwrap();
        if let Ok(_lease) = read {
            server
                .state
                .exec_store
                .put_async(
                    handle_exec_probe::EXEC_PROBE_NAMESPACE,
                    &new_key,
                    b"published while private cleanup is paused",
                )
                .await
                .unwrap();
        }
        hook.resume();
        let response = tokio::time::timeout(std::time::Duration::from_secs(10), clear)
            .await
            .unwrap()
            .unwrap();
        server.state.index_writer_shutdown.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), index_writer)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(response, Response::Cleared { .. }));
        assert!(
            publication_permitted,
            "private plan destructor retained publication exclusion"
        );
        assert!(old_entry_unreachable);
        assert!(!private.exists(), "retired private plan was not cleaned");
        assert!(
            server
                .state
                .exec_store
                .get_async(handle_exec_probe::EXEC_PROBE_NAMESPACE, &new_key)
                .await
                .unwrap()
                .is_some(),
            "Clear erased publication made during private cleanup"
        );
    })
    .await;
}

#[tokio::test]
async fn blocking_activity_cleanup_retains_root_after_operation_returns() {
    activity_cleanup(false).await;
}

#[tokio::test]
async fn blocking_activity_cleanup_retains_root_during_operation_unwind() {
    activity_cleanup(true).await;
}

async fn activity_cleanup(panic_operation: bool) {
    crate::test_support::test_timeout(async move {
        let directory = tempfile::tempdir().unwrap();
        let cache_dir = NormalizedPath::new(directory.path());
        let endpoint = crate::ipc::unique_test_endpoint();
        let server = DaemonServer::bind_with_cache_dir(&endpoint, &cache_dir).unwrap();
        let private = cache_dir.join("activity-private-plan");
        std::fs::create_dir_all(&private).unwrap();
        let output = private.join("output.rlib");
        std::fs::write(&output, b"private").unwrap();
        server.state.artifacts.insert(
            "f".repeat(64),
            CachedArtifact::from_provisional_staged(
                ArtifactIndex::new(vec!["output.rlib".into()], vec![7], vec![], vec![], 0),
                vec![output],
                Arc::new(StagedCompilePlan::for_test(private.clone(), Vec::new())),
            ),
        );
        let hook = StagedHookGuard::arm(&private, StagedHookPoint::PrivatePlanCleanup);
        let weak_state = Arc::downgrade(&server.state);
        let root = server
            .state
            .cache_root_lock
            .lease_for(STATE_TIMEOUT)
            .unwrap();
        let (resume, ready) = std::sync::mpsc::sync_channel(1);
        let worker = server.state.launch_blocking_with_root(root, move || {
            crate::core::wait::recv_for(&ready, std::time::Duration::from_secs(10)).unwrap();
            assert!(
                !panic_operation,
                "injected operation failure before activity cleanup"
            );
        });
        server.state.cache_root_lock.release();
        drop(server);
        resume.send(()).unwrap();
        let hook = tokio::task::spawn_blocking(move || {
            hook.wait_until_reached_for(std::time::Duration::from_secs(10));
            hook
        })
        .await
        .unwrap();
        let state_is_dropping = weak_state.upgrade().is_none();
        let path = cache_dir.as_path().to_path_buf();
        let root_is_owned = tokio::task::spawn_blocking(move || {
            match crate::core::cache_root_lock::CacheRootWriterLock::acquire(&path) {
                Err(error) => error.kind() == std::io::ErrorKind::WouldBlock,
                Ok(contender) => {
                    drop(contender);
                    false
                }
            }
        })
        .await
        .unwrap();
        hook.resume();
        let worker_result = tokio::time::timeout(std::time::Duration::from_secs(5), worker)
            .await
            .expect("the worker must finish its activity cleanup");
        let path = cache_dir.as_path().to_path_buf();
        let next = tokio::task::spawn_blocking(move || {
            crate::core::cache_root_lock::CacheRootWriterLock::acquire(&path)
        })
        .await
        .unwrap()
        .expect("completed activity must release native root ownership");
        drop(next);
        assert_eq!(worker_result.is_err(), panic_operation);
        assert!(
            state_is_dropping,
            "a spare state owner masked final activity destruction"
        );
        assert!(
            !private.exists(),
            "final activity did not remove its private plan"
        );
        assert!(
            root_is_owned,
            "the operation released root ownership before activity cleanup"
        );
    })
    .await;
}
