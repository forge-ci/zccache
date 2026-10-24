//! Production cleanup entrypoints must retry previously detached namespaces.
use super::*;

#[tokio::test]
async fn clear_retries_a_previously_detached_exec_namespace() {
    retry_retired_namespace(true).await;
}

#[tokio::test]
async fn maintenance_retries_a_previously_detached_exec_namespace() {
    retry_retired_namespace(false).await;
}

async fn retry_retired_namespace(clear: bool) {
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
        let key = crate::artifact::Key::from_hex(&"e".repeat(64)).unwrap();
        server
            .state
            .exec_store
            .put_async(
                handle_exec_probe::EXEC_PROBE_NAMESPACE,
                &key,
                b"abandoned namespace",
            )
            .await
            .unwrap();
        let state = Arc::clone(&server.state);
        let root = state.cache_root_lock.lease_for(STATE_TIMEOUT).unwrap();
        state
            .launch_blocking_with_root(root, {
                let state = Arc::clone(&state);
                move || {
                    drop(
                        state
                            .exec_store
                            .detach_namespace_for(
                                handle_exec_probe::EXEC_PROBE_NAMESPACE,
                                std::time::Duration::from_secs(2),
                            )
                            .unwrap(),
                    );
                }
            })
            .await
            .unwrap();
        let retired = cache_dir.join("kv/.retired");
        assert!(std::fs::read_dir(&retired).unwrap().next().is_some());
        if clear {
            assert!(matches!(
                crate::daemon::server::handle_clear::handle_clear(&state).await,
                Response::Cleared { .. }
            ));
        } else {
            maintain_state_disk(
                Arc::clone(&state),
                MaintenancePolicy::default(),
                MaintenanceKind::Full,
                None,
            )
            .await
            .unwrap();
        }
        server.state.index_writer_shutdown.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), index_writer)
            .await
            .unwrap()
            .unwrap();
        assert!(
            std::fs::read_dir(retired).unwrap().next().is_none(),
            "production cleanup never retried the abandoned namespace"
        );
    })
    .await;
}

#[tokio::test]
async fn cancelled_maintenance_owns_cleanup_after_releasing_publication() {
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
        let source = directory.path().join("old-source");
        std::fs::write(&source, b"old").unwrap();
        let key = "b".repeat(64);
        persist_staged_artifact_paths(server.state.artifact_dir.as_path(), &key, &[source.into()])
            .unwrap();
        age_tree(server.state.artifact_dir.as_path());
        let hook = StagedHookGuard::arm(
            server.state.artifact_dir.as_path(),
            StagedHookPoint::TreeSweep,
        );
        let state = Arc::clone(&server.state);
        let maintenance = tokio::spawn(async move {
            maintain_state_disk(
                state,
                MaintenancePolicy::default(),
                MaintenanceKind::Full,
                None,
            )
            .await
        });
        let hook = tokio::task::spawn_blocking(move || {
            hook.wait_until_reached_for(std::time::Duration::from_secs(10));
            hook
        })
        .await
        .unwrap();
        let publication_is_released = server.state.artifact_publication.try_read_owned().is_ok();
        let old_is_unreachable =
            load_staged_artifact_paths(server.state.artifact_dir.as_path(), &key, &[3])
                .unwrap()
                .is_none();
        maintenance.abort();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), maintenance)
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled()
        );
        let cleanup_is_owned = server.state.disk_maintenance.try_acquire().is_err();
        // Finish the WAL actor so only the cleanup worker can retain this claim.
        server.state.index_writer_shutdown.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), index_writer)
            .await
            .unwrap()
            .unwrap();
        server.state.cache_root_lock.release();
        let root = cache_dir.as_path().to_path_buf();
        let root_is_owned = tokio::task::spawn_blocking(move || {
            match crate::core::cache_root_lock::CacheRootWriterLock::acquire(&root) {
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
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let trash = server.state.artifact_dir.join(".staged-v2/.trash");
        while trash.exists() {
            assert!(
                std::time::Instant::now() < until,
                "owned maintenance did not finish"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            old_is_unreachable,
            "evicted entry remained reachable during cleanup"
        );
        assert!(
            publication_is_released,
            "maintenance retained the global writer while sweeping"
        );
        assert!(
            root_is_owned,
            "explicit daemon release let a new native writer enter during owned cleanup"
        );
        assert!(
            cleanup_is_owned,
            "request cancellation released the maintenance permit before its worker finished"
        );
    })
    .await;
}

fn age_tree(path: &Path) {
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            age_tree(&path);
        }
        kernal_api::platform::fs::set_file_mtime(
            &path,
            kernal_api::platform::fs::FileTime::from_system_time(
                std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 24 * 60 * 60),
            ),
        )
        .unwrap();
    }
}
