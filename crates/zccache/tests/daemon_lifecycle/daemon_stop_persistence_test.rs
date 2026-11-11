//! #1984: the actual CLI stop must finish persistence before a byte copy.
#![cfg(all(unix, feature = "zccache-bin"))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use zccache::core::NormalizedPath;
use zccache::daemon::DaemonServer;
use zccache::protocol::{Request, Response};
use zccache_test_support::test_child::TestChild;

struct Daemon {
    endpoint: String,
    task: tokio::task::JoinHandle<()>,
    shutdown: Arc<kernal_api::async_engine::Notify>,
    persistence: Arc<zccache::core::sync::Gate>,
}

async fn start_daemon(cache: &Path) -> Daemon {
    let cache_dir: NormalizedPath = cache.into();
    let endpoint = zccache::ipc::unique_test_endpoint();
    let mut server = DaemonServer::bind_with_cache_dir(&endpoint, &cache_dir).unwrap();
    let path = zccache::core::config::depgraph_dir_from_cache_dir(&cache_dir).join("depgraph.bin");
    if let zccache::depgraph::DepGraphLoadOutcome::Loaded { graph } =
        zccache::depgraph::classify_load(path.as_path())
    {
        server.set_dep_graph(graph);
    }
    server.artifact_store_loader().load_and_install();
    server.metadata_cache_loader().load_and_install();
    server.compiler_hash_cache_loader().load_and_install();
    let includes = server.system_includes_loader();
    tokio::task::spawn_blocking(move || includes.load_and_install())
        .await
        .unwrap();
    let shutdown = server.shutdown_handle();
    let persistence = server.test_persistence_gate();
    let task = tokio::spawn(async move { server.run(0).await.unwrap() });
    Daemon {
        endpoint,
        task,
        shutdown,
        persistence,
    }
}

async fn compile_batch(endpoint: &str, work: &Path, clang: &NormalizedPath, cached: bool) {
    for i in 0..64 {
        let source = work.join(format!("source_{i}.c"));
        let output = work.join(format!("source_{i}.o"));
        let mut client = zccache::ipc::connect(endpoint).await.unwrap();
        client
            .send(&Request::CompileEphemeral {
                client_pid: std::process::id(),
                working_dir: work.into(),
                compiler: clang.clone(),
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

fn copy_cache(source: &Path, destination: &Path) {
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

async fn stop_cli_after_endpoint_retirement(
    daemon: &Daemon,
    cache: &Path,
    delayed_writes: Vec<zccache::core::sync::GatePermit>,
) -> bool {
    let requested = daemon.shutdown.notified();
    let mut requested = std::pin::pin!(requested);
    requested.as_mut().enable();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_zccache"));
    command
        .arg("stop")
        .env("ZCCACHE_ENDPOINT", &daemon.endpoint)
        .env("ZCCACHE_CACHE_DIR", cache);
    let child = TestChild::spawn(command).unwrap();
    if tokio::time::timeout(zccache::core::wait::PROBE, requested)
        .await
        .is_err()
    {
        let output = child.collect();
        panic!(
            "CLI never requested shutdown: {:?}: {}",
            output.code,
            output.stderr_text()
        );
    }
    // Endpoint retirement alone is not durability. Keep the connected Stop
    // exchange alive while making the CLI's old teardown probe unreachable.
    std::fs::remove_file(&daemon.endpoint).unwrap();
    let mut exited = tokio::task::spawn_blocking(move || child.finish(zccache::core::wait::PROBE));
    let early = tokio::time::timeout(Duration::from_secs(1), &mut exited).await;
    let acknowledged_early = early.is_ok();
    drop(delayed_writes);
    let output = match early {
        Ok(result) => result.unwrap().unwrap(),
        Err(_) => tokio::time::timeout(zccache::core::wait::HELPER_JOIN, exited)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    };
    assert_eq!(output.code, Some(0), "{}", output.stderr_text());
    acknowledged_early
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_cli_persistence_copy_preserves_64_real_cold_compiles() {
    let clang =
        zccache::test_support::find_clang().expect("isolated compiler-test image needs clang");
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cold-cache");
    let copy = temp.path().join("copied-cache");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    for i in 0..64 {
        std::fs::write(
            work.join(format!("source_{i}.c")),
            format!("int source_{i}(void) {{ return {i}; }}\n"),
        )
        .unwrap();
    }
    let cold = start_daemon(&cache).await;
    // Hold the real workers before any cold compile: all 64 deferred payload
    // writes remain registered and unfinished when the Stop command arrives.
    let mut delayed_writes = Vec::new();
    for _ in 0..cold.persistence.available_permits() {
        delayed_writes.push(
            cold.persistence
                .acquire_for(zccache::core::wait::PROBE)
                .await,
        );
    }
    compile_batch(&cold.endpoint, &work, &clang, false).await;
    let acknowledged_early =
        stop_cli_after_endpoint_retirement(&cold, &cache, delayed_writes).await;
    copy_cache(&cache, &copy);
    tokio::time::timeout(zccache::core::wait::HELPER_JOIN, cold.task)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !acknowledged_early,
        "CLI stop returned success with 64 payload writes still delayed"
    );
    for i in 0..64 {
        std::fs::remove_file(work.join(format!("source_{i}.o"))).unwrap();
    }
    let warm = start_daemon(&copy).await;
    compile_batch(&warm.endpoint, &work, &clang, true).await;
    warm.shutdown.notify_one();
    tokio::time::timeout(zccache::core::wait::HELPER_JOIN, warm.task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_cli_budget_error_exits_nonzero() {
    let cache = tempfile::tempdir().unwrap();
    let endpoint = zccache::ipc::unique_test_endpoint();
    let mut listener = zccache::ipc::IpcListener::bind(&endpoint).unwrap();
    let daemon = tokio::spawn(async move {
        let mut connection = tokio::time::timeout(zccache::core::wait::PROBE, listener.accept())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            connection.recv::<Request>().await.unwrap(),
            Some(Request::Shutdown)
        );
        connection
            .send(&Response::Error {
                message: "shutdown_budget_exceeded: persistence drain exceeded its duration budget"
                    .into(),
            })
            .await
            .unwrap();
    });
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_zccache"));
    command
        .arg("stop")
        .env("ZCCACHE_ENDPOINT", &endpoint)
        .env("ZCCACHE_CACHE_DIR", cache.path());
    let child = TestChild::spawn(command).unwrap();
    let output = tokio::task::spawn_blocking(move || child.finish(zccache::core::wait::PROBE))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(output.code, Some(0), "a budget failure must exit nonzero");
    assert!(
        output.stderr_text().contains("shutdown_budget_exceeded"),
        "{}",
        output.stderr_text()
    );
    tokio::time::timeout(zccache::core::wait::HELPER_JOIN, daemon)
        .await
        .unwrap()
        .unwrap();
}
