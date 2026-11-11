//! A real host process dies after per-entry commits, without running Drop.

use super::*;
use std::time::Duration;
use zccache_test_support::test_child::{Readiness, TestChild};

const CHILD_REQUEST: &str = "ZCCACHE_TEST_INCREMENTAL_CRASH_REQUEST";

#[tokio::test]
async fn crash_persistence_child() {
    let Ok(encoded) = std::env::var(CHILD_REQUEST) else {
        return;
    };
    let (root, compiler, args, work, marker): (PathBuf, PathBuf, Vec<String>, PathBuf, PathBuf) =
        serde_json::from_str(&encoded).unwrap();
    let service = start_service(&root).await;
    let request = CompileRequest {
        audit: AuditContext::new(
            crate::audit::AuditId::new("crash-run").unwrap(),
            crate::audit::AuditId::new("crash-trace").unwrap(),
        ),
        compiler: compiler.into(),
        args,
        cwd: work.into(),
        env: Vec::new(),
        stdin: Vec::new(),
    };
    compile_once(&service, request).await;
    let actual_root = service.stats().await.unwrap().cache_root;
    std::fs::write(marker, actual_root.to_string_lossy().as_bytes()).unwrap();
    // Parent waits for both independent commits, then kills this process.
    // No explicit flush, shutdown or destructor is allowed before that kill.
    kernal_api::async_engine::sleep(Duration::from_secs(90)).await;
    panic!("parent did not kill the crash fixture within its deadline");
}

fn committed_root(marker: &std::path::Path) -> Option<PathBuf> {
    let root = PathBuf::from(std::fs::read_to_string(marker).ok()?);
    let normalized = crate::core::NormalizedPath::new(&root);
    let index = crate::core::config::index_path_from_cache_dir(&normalized);
    let graph = crate::core::config::depgraph_dir_from_cache_dir(&normalized).join("depgraph.bin");
    let store = crate::artifact::ArtifactStore::open(index.as_path()).ok()?;
    let graph = crate::depgraph::load_from_file(graph.as_path()).ok()?;
    (!store.is_empty() && graph.contexts_with_artifact_key() > 0).then_some(root)
}

/// #1954: committed entries survive SIGKILL without either whole-store checkpoint.
#[tokio::test]
async fn committed_compile_hits_after_host_sigkill_without_a_checkpoint() {
    let Some(compiler) = crate::test_support::find_clang() else {
        return;
    };
    let fx = fixture(compiler);
    let marker = fx._temp.path().join("compiled-root");
    let encoded = serde_json::to_string(&(
        &fx.cache_root,
        fx.request.compiler.as_path(),
        &fx.request.args,
        fx.request.cwd.as_path(),
        &marker,
    ))
    .unwrap();
    let (root, output) = kernal_api::async_engine::launch_blocking(move || {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "embedded::metadata_persist_tests::incremental_crash::crash_persistence_child",
                "--nocapture",
            ])
            .env(CHILD_REQUEST, encoded);
        let mut child = TestChild::spawn(command).unwrap();
        let readiness = child
            .wait_until(Duration::from_secs(60), || {
                committed_root(&marker).is_some()
            })
            .unwrap();
        let root = committed_root(&marker);
        child.terminate().unwrap();
        let output = child.collect();
        assert_eq!(
            readiness,
            Readiness::Ready,
            "child stderr: {}",
            output.stderr_text()
        );
        (root.unwrap(), output)
    })
    .await
    .unwrap();
    assert!(
        output.code != Some(0),
        "the child must be killed, not exit cleanly"
    );
    let normalized = crate::core::NormalizedPath::new(&root);
    assert!(!crate::core::config::index_path_from_cache_dir(&normalized).exists());
    assert!(
        !crate::core::config::depgraph_dir_from_cache_dir(&normalized)
            .join("depgraph.bin")
            .exists()
    );
    std::fs::remove_file(fx.source.with_extension("o")).unwrap();
    let service = start_service(&fx.cache_root).await;
    let response = service.compile(fx.request).await.unwrap();
    assert_eq!(response.exit_code, 0);
    assert!(response.cached, "committed entries must survive SIGKILL");
    service.shutdown(ShutdownMode::Graceful).await.unwrap();
}
