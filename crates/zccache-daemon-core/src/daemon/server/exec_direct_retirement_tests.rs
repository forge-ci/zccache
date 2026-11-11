//! Direct exec must hand off publication before independently retiring old records.
use super::super::*;
use crate::daemon::server::persist::{StagedHookGuard, StagedHookPoint};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_exec_cleanup_does_not_retain_publication_or_its_async_caller() {
    crate::test_support::test_timeout(async {
        let directory = tempfile::tempdir().unwrap();
        let server = crate::daemon::server::tests::bind_isolated_server(directory.path());
        let private = server.state.cache_dir.join("direct-exec-private-plan");
        std::fs::create_dir_all(&private).unwrap();
        let output = private.join("old.out");
        std::fs::write(&output, b"private").unwrap();
        let key = "a".repeat(64);
        server.state.artifacts.insert(
            key.clone(),
            CachedArtifact::from_provisional_staged(
                ArtifactIndex::new(vec!["old.out".into()], vec![7], vec![], vec![], 0),
                vec![output],
                Arc::new(StagedCompilePlan::for_test(private.clone(), Vec::new())),
            ),
        );
        let hook = StagedHookGuard::arm(&private, StagedHookPoint::PrivatePlanCleanup);
        let state = Arc::clone(&server.state);
        let replacement_key = key.clone();
        let mut store = tokio::spawn(async move {
            handle_exec::store_exec_artifact(
                &state,
                replacement_key,
                ArtifactData {
                    outputs: vec![ArtifactOutput {
                        name: "new.out".into(),
                        payload: ArtifactPayload::Bytes(Arc::new(b"new bytes".to_vec())),
                    }],
                    stdout: Arc::new(b"replacement".to_vec()),
                    stderr: Arc::new(Vec::new()),
                    exit_code: 0,
                },
                None,
            )
            .await
        });
        let hook = tokio::task::spawn_blocking(move || {
            hook.wait_until_reached_for(Duration::from_secs(10));
            hook
        })
        .await
        .unwrap();
        // Observe completion while cleanup remains paused. The actual payload
        // persistence and publication release must proceed independently.
        let completed = tokio::time::timeout(Duration::from_secs(2), &mut store).await;
        let caller_proceeded = completed.is_ok();
        if let Ok(result) = completed {
            assert!(result.unwrap().unwrap().is_none());
        }
        let writer = tokio::time::timeout(
            Duration::from_secs(2),
            server
                .state
                .artifact_publication
                .drain_for(Duration::from_secs(5)),
        )
        .await;
        let writer_proceeded = writer.is_ok();
        drop(writer);
        let replacement_visible = writer_proceeded
            && server
                .state
                .artifacts
                .get(&key)
                .is_some_and(|entry| entry.stdout.as_slice() == b"replacement");
        let root = server.state.cache_dir.clone();
        server.state.cache_root_lock.release();
        let contention_root = root.clone();
        let contender = tokio::task::spawn_blocking(move || {
            crate::core::cache_root_lock::CacheRootWriterLock::acquire(&contention_root)
        })
        .await
        .unwrap();
        let root_retained = contender
            .as_ref()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock);
        drop(contender);
        let lifetime = Arc::downgrade(&server.state);
        drop(server);
        let state_released = lifetime.upgrade().is_none();
        hook.resume();
        if !caller_proceeded {
            assert!(tokio::time::timeout(Duration::from_secs(10), store)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .is_none());
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while private.exists() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            caller_proceeded,
            "direct exec kept its async caller in private cleanup"
        );
        assert!(writer_proceeded, "direct exec cleanup retained publication");
        assert!(replacement_visible);
        assert!(
            root_retained,
            "explicit shutdown release bypassed active cleanup ownership"
        );
        assert!(
            state_released,
            "cleanup must retain its narrow root lease without retaining the full cache state"
        );
        let next = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let attempt_root = root.clone();
                let attempt = tokio::task::spawn_blocking(move || {
                    crate::core::cache_root_lock::CacheRootWriterLock::acquire(&attempt_root)
                })
                .await
                .unwrap();
                match attempt {
                    Ok(writer) => break writer,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(error) => panic!("cache root reacquisition failed: {error}"),
                }
            }
        })
        .await
        .unwrap();
        drop(next);
    })
    .await;
}
