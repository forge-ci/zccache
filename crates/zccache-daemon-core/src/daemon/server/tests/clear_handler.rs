//! Tests for `handle_clear` — Request::Clear's preservation invariants.
//!
//! Issue #558: `system_includes` (and the sibling `compiler_hash_cache`)
//! store compiler-environment data keyed by `(compiler_path, mtime, size)`.
//! They are self-correcting via stat-verify on every access, so wiping
//! them across Clear pays the ~44 ms re-probe / ~50–60 ms re-hash penalty
//! on the next compile while contributing nothing to the user's intent
//! of clearing built artifacts.

use super::super::*;

#[path = "clear_handler_provisional.rs"]
mod provisional;
#[path = "clear_handler_recovery.rs"]
mod recovery;

#[tokio::test]
async fn handle_clear_releases_publication_before_kv_sweep() {
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
        let old_key = crate::artifact::Key::from_hex(&"a".repeat(64)).unwrap();
        server
            .state
            .exec_store
            .put_async(
                handle_exec_probe::EXEC_PROBE_NAMESPACE,
                &old_key,
                b"old before Clear",
            )
            .await
            .unwrap();
        let hook = StagedHookGuard::arm(cache_dir.as_path(), StagedHookPoint::KvNamespaceSweep);
        let state = Arc::clone(&server.state);
        let clear =
            tokio::spawn(async move { super::super::handle_clear::handle_clear(&state).await });
        let hook = tokio::task::spawn_blocking(move || {
            hook.wait_until_reached_for(std::time::Duration::from_secs(10));
            hook
        })
        .await
        .unwrap();
        let read = server.state.artifact_publication.try_read_owned();
        let publication_permitted = read.is_ok();
        if let Ok(_lease) = read {
            let key = crate::artifact::Key::from_hex(&"c".repeat(64)).unwrap();
            server
                .state
                .exec_store
                .put_async(
                    handle_exec_probe::EXEC_PROBE_NAMESPACE,
                    &key,
                    b"new after Clear",
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
            .expect("index writer must drain after shutdown")
            .unwrap();
        assert!(matches!(response, Response::Cleared { .. }));
        assert!(server
            .state
            .exec_store
            .get_async(handle_exec_probe::EXEC_PROBE_NAMESPACE, &old_key,)
            .await
            .unwrap()
            .is_none());
        assert!(
            publication_permitted,
            "Clear retained the publication writer during recursive KV cleanup"
        );
        let key = crate::artifact::Key::from_hex(&"c".repeat(64)).unwrap();
        assert_eq!(
            server
                .state
                .exec_store
                .get_async(handle_exec_probe::EXEC_PROBE_NAMESPACE, &key)
                .await
                .unwrap()
                .unwrap(),
            b"new after Clear"
        );
    })
    .await;
}

#[tokio::test]
async fn cancelled_clear_keeps_its_worker_owned_until_cleanup_finishes() {
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
        let old_key = crate::artifact::Key::from_hex(&"a".repeat(64)).unwrap();
        server
            .state
            .exec_store
            .put_async(
                handle_exec_probe::EXEC_PROBE_NAMESPACE,
                &old_key,
                b"old before Clear",
            )
            .await
            .unwrap();
        let hook = StagedHookGuard::arm(cache_dir.as_path(), StagedHookPoint::KvNamespaceSweep);
        let state = Arc::clone(&server.state);
        let clear =
            tokio::spawn(async move { super::super::handle_clear::handle_clear(&state).await });
        let hook = tokio::task::spawn_blocking(move || {
            hook.wait_until_reached_for(std::time::Duration::from_secs(10));
            hook
        })
        .await
        .unwrap();
        clear.abort();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), clear)
                .await
                .expect("Clear cancellation must return")
                .unwrap_err()
                .is_cancelled()
        );
        let cleanup_is_owned = server.state.disk_maintenance.try_acquire().is_err();
        let publication_is_released = server.state.artifact_publication.try_read_owned().is_ok();
        // Finish the WAL actor so only the cleanup worker can retain this claim.
        server.state.index_writer_shutdown.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), index_writer)
            .await
            .expect("index writer must drain after shutdown")
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
        while server.state.disk_maintenance.try_acquire().is_err() {
            assert!(
                std::time::Instant::now() < until,
                "cancelled Clear did not finish owned cleanup"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            root_is_owned,
            "explicit daemon release let a new native writer enter during owned cleanup"
        );
        assert!(
            cleanup_is_owned,
            "cancelling Clear released its maintenance permit before the blocking worker finished"
        );
        assert!(
            publication_is_released,
            "recursive cleanup retained publication exclusion"
        );
    })
    .await;
}

/// Unit-level invariant: an entry stored in `SystemIncludeCache` survives
/// being "skipped by Clear" cleanly. The follow-up `get` stat-verifies the
/// compiler binary; an unchanged binary returns the cached entry, and any
/// post-Clear change to the compiler (the only correctness concern with
/// preservation) is detected on the next access and rejected.
///
/// This is the safety net that makes [`handle_clear`] safe to skip
/// `system_includes.clear()` (and the symmetric `compiler_hash_cache`
/// already gets this treatment — see issue #517).
#[test]
fn system_include_cache_entry_self_verifies_after_clear_skip() {
    use crate::depgraph::SystemIncludeCache;

    let tmp = tempfile::tempdir().unwrap();
    let compiler = tmp.path().join("clang");
    std::fs::write(&compiler, b"compiler binary").unwrap();
    let include_dir = tmp.path().join("usr-include");
    std::fs::create_dir_all(&include_dir).unwrap();

    let mut cache = SystemIncludeCache::new();
    cache.insert(
        crate::core::NormalizedPath::new(&compiler),
        vec![crate::core::NormalizedPath::new(&include_dir)],
    );
    assert_eq!(cache.len(), 1);

    // "Clear ran but system_includes was NOT wiped" — the entry must
    // still stat-verify against the unchanged compiler.
    let hit = cache.get(&compiler);
    assert!(
        hit.is_some(),
        "preserved entry must still stat-verify against an unchanged compiler"
    );
    assert_eq!(hit.unwrap().len(), 1);

    // After a compiler change, stat-verify must reject the entry —
    // this is the safety net that makes preservation across Clear safe.
    kernal_api::platform::fs::set_file_mtime(
        &compiler,
        kernal_api::platform::fs::FileTime::from_unix_time(2_000_000_000, 0),
    )
    .unwrap();
    std::fs::write(&compiler, b"different compiler bytes after upgrade").unwrap();
    let post_change = cache.get(&compiler);
    assert!(
        post_change.is_none(),
        "stat-verify must reject the entry once the compiler binary changes"
    );
}

/// Integration-level check: after Clear, the in-memory
/// `system_includes` cache is NOT empty if it was non-empty before.
/// Uses the `#[cfg(test)]` `test_insert_system_includes` /
/// `test_system_includes_len` seams to pre-populate and observe
/// without standing up a full compile pipeline (which would require
/// clang on PATH and would couple the test to the chosen toolchain).
#[tokio::test]
#[ignore] // integration-level: instantiates a real DaemonServer
async fn handle_clear_preserves_system_includes() {
    crate::test_support::test_timeout(async {
        let endpoint = crate::ipc::unique_test_endpoint();
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = crate::core::NormalizedPath::new(tmp.path());
        let mut server = DaemonServer::bind_with_cache_dir(&endpoint, &cache_dir).unwrap();
        // `handle_clear` waits for the index writer to acknowledge its durable
        // Clear command; outside `DaemonServer::run` nothing drains that queue,
        // so without this worker the call never returns (#1648).
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

        let fake_compiler = tmp.path().join("fake-clang");
        std::fs::write(&fake_compiler, b"fake compiler bytes").unwrap();
        let synthetic_include_dir = tmp.path().join("include");
        std::fs::create_dir_all(&synthetic_include_dir).unwrap();
        server
            .test_insert_system_includes(
                crate::core::NormalizedPath::new(&fake_compiler),
                vec![crate::core::NormalizedPath::new(&synthetic_include_dir)],
            )
            .await;
        assert_eq!(
            server.test_system_includes_len().await,
            1,
            "test setup: synthetic entry must be installed"
        );

        // Drive handle_clear via the same internal call site that the
        // request handler uses. This bypasses IPC so we retain access
        // to the server to observe state after Clear.
        let response = super::super::handle_clear::handle_clear(&server.test_state_arc()).await;
        assert!(
            matches!(response, Response::Cleared { .. }),
            "expected Cleared response, got: {response:?}"
        );

        assert_eq!(
            server.test_system_includes_len().await,
            1,
            "issue #558: handle_clear must preserve system_includes entries — \
             they self-verify via stat-verify and re-discovery is expensive"
        );
        server.state.index_writer_shutdown.notify_waiters();
        index_writer.await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn handle_clear_preserves_in_flight_private_staging() {
    crate::test_support::test_timeout(async {
        let endpoint = crate::ipc::unique_test_endpoint();
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = crate::core::NormalizedPath::new(tmp.path());
        let mut server = DaemonServer::bind_with_cache_dir(&endpoint, &cache_dir).unwrap();
        // `handle_clear` normally runs inside `DaemonServer::run`, where the
        // index writer consumes and acknowledges its durable Clear command.
        // This direct unit-level invocation must start that same worker.
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
        let staged = server.test_state().staging.path().join("active-output.o");
        std::fs::write(&staged, b"compiler result").unwrap();
        let published = tmp.path().join("published-output.o");
        std::fs::write(&published, b"cached result").unwrap();
        let key = "6".repeat(64);
        persist_staged_artifact_paths(
            server.test_state().artifact_dir.as_path(),
            &key,
            &[published.into()],
        )
        .unwrap();
        let legacy_key = "5".repeat(64);
        let pack_key = "4".repeat(64);
        let legacy_path = server
            .test_state()
            .artifact_dir
            .join(format!("{legacy_key}_0"));
        let pack_path = pack_path_for(server.test_state().artifact_dir.as_path(), &pack_key);
        std::fs::write(&legacy_path, b"legacy result").unwrap();
        std::fs::write(
            &pack_path,
            build_pack(&[Arc::new(b"packed result".to_vec())]),
        )
        .unwrap();

        let response = super::super::handle_clear::handle_clear(&server.test_state_arc()).await;
        assert!(matches!(
            response,
            Response::Cleared {
                on_disk_bytes_freed,
                ..
            } if on_disk_bytes_freed >= 13
        ));
        assert_eq!(
            std::fs::read(&staged).unwrap(),
            b"compiler result",
            "Clear must not delete a compiler result before salvage/materialization"
        );
        assert!(load_staged_artifact_paths(
            server.test_state().artifact_dir.as_path(),
            &key,
            &[13],
        )
        .unwrap()
        .is_none());
        assert!(!legacy_path.exists());
        assert!(!pack_path.exists());
        server.state.index_writer_shutdown.notify_waiters();
        index_writer.await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn handle_clear_cannot_be_overtaken_by_delayed_startup_hydration() {
    crate::test_support::test_timeout(async {
        let endpoint = crate::ipc::unique_test_endpoint();
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = crate::core::NormalizedPath::new(tmp.path());
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
        let key = "3".repeat(64);
        let meta = ArtifactIndex::new(
            vec!["old.o".to_string()],
            vec![16],
            Vec::new(),
            Vec::new(),
            0,
        );
        server.state.artifact_store.insert(&key, &meta);
        std::fs::write(
            server.state.artifact_dir.join(format!("{key}_0")),
            b"old startup artifact",
        )
        .unwrap();

        let gate = Arc::new(Notify::new());
        let loader = super::super::run::spawn_artifact_loader(
            Arc::clone(&server.state),
            Some(Arc::clone(&gate)),
        )
        .await;
        let clear_state = Arc::clone(&server.state);
        let mut clear =
            tokio::spawn(
                async move { super::super::handle_clear::handle_clear(&clear_state).await },
            );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut clear)
                .await
                .is_err(),
            "Clear must wait for startup hydration that already owns publication"
        );

        gate.notify_one();
        loader.await.unwrap();
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), clear)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(response, Response::Cleared { .. }));
        assert!(!server.state.artifacts.contains_key(&key));
        assert!(server.state.artifact_store.get(&key).is_none());
        assert!(!server.state.artifact_dir.join(format!("{key}_0")).exists());

        server.state.index_writer_shutdown.notify_waiters();
        index_writer.await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn handle_clear_waits_for_owned_cache_lookup_lease() {
    crate::test_support::test_timeout(async {
        let endpoint = crate::ipc::unique_test_endpoint();
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = crate::core::NormalizedPath::new(tmp.path());
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
        let key = "7".repeat(64);
        let meta = ArtifactIndex::new(
            vec!["leased.o".to_string()],
            vec![14],
            Vec::new(),
            Vec::new(),
            0,
        );
        server.state.artifact_store.insert(&key, &meta);
        server
            .state
            .artifacts
            .insert(key.clone(), CachedArtifact::from_index(meta));
        let payload_path = server.state.artifact_dir.join(format!("{key}_0"));
        std::fs::write(&payload_path, b"leased payload").unwrap();

        let lookup = lookup_artifact_with_disk_fallback(&server.state, &key)
            .expect("live artifact should acquire a publication lease");
        let clear_state = Arc::clone(&server.state);
        let mut clear =
            tokio::spawn(
                async move { super::super::handle_clear::handle_clear(&clear_state).await },
            );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut clear)
                .await
                .is_err(),
            "Clear must wait for an owned lookup to finish materializing"
        );
        record_artifact_access(&server.state, &key, &lookup, Instant::now());
        drop(lookup);

        let response = tokio::time::timeout(std::time::Duration::from_secs(5), clear)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(response, Response::Cleared { .. }));
        assert!(!server.state.artifacts.contains_key(&key));
        assert!(server.state.artifact_store.get(&key).is_none());
        assert!(!payload_path.exists());

        server.state.index_writer_shutdown.notify_waiters();
        index_writer.await.unwrap();
    })
    .await;
}
