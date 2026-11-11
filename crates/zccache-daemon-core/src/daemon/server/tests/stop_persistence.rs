//! #1984: the existing Shutdown response is a durability barrier.

use super::super::*;

#[tokio::test]
async fn stop_persistence_accepts_stdout_only_exec_and_releases_writer() {
    let cache = tempfile::tempdir().unwrap();
    let endpoint = crate::ipc::unique_test_endpoint();
    let mut server = super::bind_isolated_server_at(&endpoint, cache.path());
    let state = server.test_state_arc();
    let task = tokio::spawn(async move { server.run(0).await.unwrap() });
    let artifact = ArtifactData {
        outputs: Vec::new(),
        stdout: Arc::new(b"stdout-only result".to_vec()),
        stderr: Arc::new(Vec::new()),
        exit_code: 0,
    };
    handle_exec::store_exec_artifact(&state, "cd".repeat(32), artifact, None)
        .await
        .unwrap();
    let mut client = crate::ipc::connect(&endpoint).await.unwrap();
    client.send(&Request::Shutdown).await.unwrap();
    assert_eq!(
        client.recv::<Response>().await.unwrap(),
        Some(Response::ShuttingDown)
    );
    tokio::time::timeout(crate::core::wait::HELPER_JOIN, task)
        .await
        .unwrap()
        .unwrap();
    let endpoint = crate::ipc::unique_test_endpoint();
    let _restarted = super::bind_isolated_server_at(&endpoint, cache.path());
}

#[tokio::test]
async fn stop_persistence_response_waits_for_detached_publishers() {
    let cache = tempfile::tempdir().unwrap();
    let endpoint = crate::ipc::unique_test_endpoint();
    let mut server = super::bind_isolated_server_at(&endpoint, cache.path());
    let state = server.test_state_arc();
    // A controlled persistence delay, using the same permit owned by detached
    // compile, link and exec writers. No process-global delay hook is needed.
    let publisher = begin_artifact_publication(&state).await.unwrap();
    let task = tokio::spawn(async move { server.run(0).await.unwrap() });
    let mut client = crate::ipc::connect(&endpoint).await.unwrap();
    client.send(&Request::Shutdown).await.unwrap();
    let mut response = Box::pin(client.recv::<Response>());
    let early = tokio::time::timeout(Duration::from_millis(150), &mut response).await;
    let acknowledged_early = early.is_ok();
    drop(publisher);
    let final_response = match early {
        Ok(result) => result.unwrap(),
        Err(_) => tokio::time::timeout(crate::core::wait::HELPER_JOIN, response)
            .await
            .unwrap()
            .unwrap(),
    };
    tokio::time::timeout(crate::core::wait::HELPER_JOIN, task)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !acknowledged_early,
        "Shutdown acknowledged an unfinished publisher"
    );
    assert_eq!(final_response, Some(Response::ShuttingDown));
}

#[tokio::test]
async fn stop_persistence_writer_does_not_acknowledge_disk_failure() {
    let cache = tempfile::tempdir().unwrap();
    let endpoint = crate::ipc::unique_test_endpoint();
    let mut server = super::bind_isolated_server_at(&endpoint, cache.path());
    let state = server.test_state_arc();
    // A real filesystem error: the journal cannot create namespaces below kv.
    let index = crate::core::config::index_path_from_cache_dir(&state.cache_dir);
    std::fs::create_dir_all(index.as_path().parent().unwrap()).unwrap();
    std::fs::write(
        index.as_path().with_extension("entries"),
        b"not a directory",
    )
    .unwrap();
    let row = ArtifactIndex::new(Vec::new(), Vec::new(), Vec::new(), Vec::new(), 0);
    state.artifact_store.insert("failed-row", &row);
    let writer = super::multi_restart_context_key::spawn_index_writer(&mut server);
    let flushed = flush_index_writer(&state.index_writer_tx, Duration::from_secs(5)).await;
    state.index_writer_shutdown.notify_one();
    tokio::time::timeout(crate::core::wait::HELPER_JOIN, writer)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !flushed,
        "failed journal publication must not acknowledge success"
    );
}

#[cfg(unix)]
async fn start_on(cache: &std::path::Path) -> (String, tokio::task::JoinHandle<()>) {
    let endpoint = crate::ipc::unique_test_endpoint();
    let mut server = super::bind_isolated_server_at(&endpoint, cache);
    let path = depgraph_file_path_for_cache_dir(&server.state.cache_dir);
    if let crate::depgraph::DepGraphLoadOutcome::Loaded { graph } =
        crate::depgraph::classify_load(path.as_path())
    {
        server.set_dep_graph(graph);
    }
    server.artifact_store_loader().load_and_install();
    server.metadata_cache_loader().load_and_install();
    server.compiler_hash_cache_loader().load_and_install();
    let task = tokio::spawn(async move { server.run(0).await.unwrap() });
    (endpoint, task)
}

#[cfg(unix)]
async fn compile_batch(endpoint: &str, work: &std::path::Path, cc: &std::path::Path, cached: bool) {
    for i in 0..64 {
        let source = work.join(format!("source_{i}.c"));
        let output = work.join(format!("source_{i}.o"));
        let mut client = crate::ipc::connect(endpoint).await.unwrap();
        client
            .send(&Request::CompileEphemeral {
                client_pid: std::process::id(),
                working_dir: work.into(),
                compiler: cc.into(),
                args: vec![
                    "-c".into(),
                    source.to_string_lossy().into_owned(),
                    "-o".into(),
                    output.to_string_lossy().into_owned(),
                ],
                cwd: work.into(),
                env: None,
                stdin: Vec::new(),
            })
            .await
            .unwrap();
        let response = client.recv::<Response>().await.unwrap();
        assert!(
            matches!(response, Some(Response::CompileResult { exit_code: 0, cached: actual, .. }) if actual == cached),
            "compile {i}, expected cached={cached}: {response:?}"
        );
    }
}

#[cfg(unix)]
fn copy_cache(source: &std::path::Path, destination: &std::path::Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_cache(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn stop_persistence_copy_immediately_after_response_preserves_64_cold_compiles() {
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cold-cache");
    let copy = temp.path().join("copied-cache");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let cc = super::multi_restart_context_key::write_fake_multi_cc(temp.path());
    for i in 0..64 {
        std::fs::write(
            work.join(format!("source_{i}.c")),
            format!("int source_{i}(void) {{ return {i}; }}\n"),
        )
        .unwrap();
    }
    let (endpoint, cold) = start_on(&cache).await;
    compile_batch(&endpoint, &work, &cc, false).await;
    let mut client = crate::ipc::connect(&endpoint).await.unwrap();
    client.send(&Request::Shutdown).await.unwrap();
    assert_eq!(
        client.recv::<Response>().await.unwrap(),
        Some(Response::ShuttingDown)
    );
    // Do not join the daemon before copying: the response itself is the contract.
    copy_cache(&cache, &copy);
    tokio::time::timeout(crate::core::wait::HELPER_JOIN, cold)
        .await
        .unwrap()
        .unwrap();
    for i in 0..64 {
        std::fs::remove_file(work.join(format!("source_{i}.o"))).unwrap();
    }
    let (endpoint, warm) = start_on(&copy).await;
    compile_batch(&endpoint, &work, &cc, true).await;
    let mut client = crate::ipc::connect(&endpoint).await.unwrap();
    client.send(&Request::Shutdown).await.unwrap();
    assert_eq!(
        client.recv::<Response>().await.unwrap(),
        Some(Response::ShuttingDown)
    );
    tokio::time::timeout(crate::core::wait::HELPER_JOIN, warm)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn stop_persistence_budget_exceeded_returns_typed_error() {
    let cache = tempfile::tempdir().unwrap();
    let endpoint = crate::ipc::unique_test_endpoint();
    let mut server = super::bind_isolated_server_at(&endpoint, cache.path());
    Arc::get_mut(&mut server.state)
        .unwrap()
        .shutdown_completion
        .budget = Duration::from_millis(50);
    let state = server.test_state_arc();
    let publisher = begin_artifact_publication(&state).await.unwrap();
    let task = tokio::spawn(async move { server.run(0).await.unwrap() });
    let mut client = crate::ipc::connect(&endpoint).await.unwrap();
    client.send(&Request::Shutdown).await.unwrap();
    let response = tokio::time::timeout(crate::core::wait::PROBE, client.recv::<Response>())
        .await
        .unwrap()
        .unwrap();
    drop(publisher);
    assert_eq!(
        response,
        Some(shutdown_completion::ShutdownError::BudgetExceeded.response())
    );
    tokio::time::timeout(crate::core::wait::HELPER_JOIN, task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn stop_persistence_disk_failure_returns_typed_error() {
    let cache = tempfile::tempdir().unwrap();
    let endpoint = crate::ipc::unique_test_endpoint();
    let mut server = super::bind_isolated_server_at(&endpoint, cache.path());
    let state = server.test_state_arc();
    let index = crate::core::config::index_path_from_cache_dir(&state.cache_dir);
    std::fs::write(
        index.as_path().with_extension("entries"),
        b"not a directory",
    )
    .unwrap();
    let row = ArtifactIndex::new(Vec::new(), Vec::new(), Vec::new(), Vec::new(), 0);
    state.artifact_store.insert("failed-row", &row);
    let task = tokio::spawn(async move { server.run(0).await.unwrap() });
    let mut client = crate::ipc::connect(&endpoint).await.unwrap();
    client.send(&Request::Shutdown).await.unwrap();
    let response = client.recv::<Response>().await.unwrap();
    assert_eq!(
        response,
        Some(shutdown_completion::ShutdownError::Persistence.response())
    );
    tokio::time::timeout(crate::core::wait::HELPER_JOIN, task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn stop_persistence_accepts_durable_legacy_packed_hits() {
    let cache = tempfile::tempdir().unwrap();
    let endpoint = crate::ipc::unique_test_endpoint();
    let mut server = super::bind_isolated_server_at(&endpoint, cache.path());
    let state = server.test_state_arc();
    let key = "ab".repeat(32);
    let bytes = Arc::new(b"durable legacy object".to_vec());
    let metadata = ArtifactIndex::new(
        vec!["object.o".into()],
        vec![bytes.len() as u64],
        Vec::new(),
        Vec::new(),
        0,
    );
    std::fs::create_dir_all(&state.artifact_dir).unwrap();
    std::fs::write(
        pack_path_for(&state.artifact_dir, &key),
        build_pack(&[bytes]),
    )
    .unwrap();
    let cached = CachedArtifact::from_index(metadata.clone());
    let payloads =
        ensure_payloads_with_staged_policy(&cached, &state.artifact_dir, &key, false).unwrap();
    assert!(
        matches!(&payloads[0], CachedPayload::Bytes(_)),
        "legacy packs decode to resident bytes"
    );
    state.artifact_store.insert(&key, &metadata);
    state.artifacts.insert(key, cached);
    let task = tokio::spawn(async move { server.run(0).await.unwrap() });
    let mut client = crate::ipc::connect(&endpoint).await.unwrap();
    client.send(&Request::Shutdown).await.unwrap();
    assert_eq!(
        client.recv::<Response>().await.unwrap(),
        Some(Response::ShuttingDown)
    );
    tokio::time::timeout(crate::core::wait::HELPER_JOIN, task)
        .await
        .unwrap()
        .unwrap();
}
