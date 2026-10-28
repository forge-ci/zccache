//! Integration test for rustc `@argfile` compiles through the daemon.
//!
//! Cargo runs `rustc @argfile` when a command line is too long for Windows,
//! one argument per line. rustc reads each line verbatim, so the daemon must
//! expand the file the same way: the GCC grammar split
//! `cfg(feature, values("a", "b"))` and rustc failed with "multiple input
//! filenames provided" (ffmpeg-sys-next, 776 `--cfg` flags).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unwrap_in_result
)]

use zccache::core::NormalizedPath;
use zccache::daemon::DaemonServer;
use zccache::protocol::{Request, Response};

#[cfg(unix)]
type ClientConn = zccache::ipc::IpcConnection;
#[cfg(windows)]
type ClientConn = zccache::ipc::IpcClientConnection;

/// Version cfgs in the argfile: enough that the expanded command line passes
/// the daemon's 30,000-character spill threshold, like ffmpeg-sys-next.
const VERSION_CFGS: usize = 800;

async fn start_daemon() -> (
    String,
    tokio::task::JoinHandle<()>,
    std::sync::Arc<kernal_api::async_engine::Notify>,
) {
    let endpoint = zccache::ipc::unique_test_endpoint();
    // Isolated cache root (#1648), owned by the server task.
    let cache_root = tempfile::tempdir().expect("daemon cache tempdir");
    let cache_dir: zccache::core::NormalizedPath = cache_root.path().join("zccache-cache").into();
    let mut server = DaemonServer::bind_with_cache_dir(&endpoint, &cache_dir).unwrap();
    let shutdown = server.shutdown_handle();
    let handle = tokio::spawn(async move {
        let _cache_root = cache_root;
        server.run(0).await.unwrap();
    });
    (endpoint, handle, shutdown)
}

async fn start_session(client: &mut ClientConn) -> String {
    client
        .send(&Request::SessionStart {
            client_pid: std::process::id(),
            working_dir: std::env::current_dir().unwrap().into(),
            log_file: None,
            track_stats: false,
            journal_path: None,
            profile: false,
            private_daemon: None,
        })
        .await
        .unwrap();

    match client.recv().await.unwrap() {
        Some(Response::SessionStarted { session_id, .. }) => session_id,
        other => panic!("expected SessionStarted, got: {other:?}"),
    }
}

/// Compile via IPC and return (exit_code, cached, stderr).
async fn compile(
    client: &mut ClientConn,
    session_id: &str,
    compiler: &str,
    args: &[String],
    cwd: &std::path::Path,
) -> (i32, bool, String) {
    client
        .send(&Request::Compile {
            session_id: session_id.to_string(),
            args: args.to_vec(),
            cwd: cwd.to_path_buf().into(),
            compiler: NormalizedPath::new(compiler),
            env: None,
            stdin: Vec::new(),
        })
        .await
        .unwrap();

    // Skip the #1216 `CompileProgress` heartbeats.
    loop {
        match client.recv().await.unwrap() {
            Some(Response::CompileProgress { .. }) => continue,
            Some(Response::CompileResult {
                exit_code,
                cached,
                stderr,
                ..
            }) => {
                return (
                    exit_code,
                    cached,
                    String::from_utf8_lossy(&stderr).into_owned(),
                )
            }
            Some(Response::Error { message }) => panic!("compile error: {message}"),
            other => panic!("unexpected response: {other:?}"),
        }
    }
}

/// Write a cargo-style argfile (one argument per line) for `src`.
fn write_cargo_argfile(dir: &std::path::Path, src: &str, output: &str) -> std::path::PathBuf {
    let mut lines: Vec<String> = [
        "--crate-name",
        "argfile",
        "--edition=2021",
        "--crate-type",
        "lib",
        "--emit=link",
        src,
        "-o",
        output,
        "-D",
        "warnings",
        "--check-cfg",
        r#"cfg(feature, values("avcodec", "avdevice"))"#,
        "--cfg",
        r#"feature="avcodec""#,
    ]
    .iter()
    .map(|arg| (*arg).to_string())
    .collect();
    for i in 0..VERSION_CFGS {
        lines.push("--check-cfg".to_string());
        lines.push(format!("cfg(version_greater_than_{i})"));
        lines.push("--cfg".to_string());
        lines.push(format!("version_greater_than_{i}"));
    }
    let argfile = dir.join("cargo-argfile");
    std::fs::write(&argfile, lines.join("\n") + "\n").unwrap();
    argfile
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore] // integration-level: starts real daemon with IPC + rustc
async fn test_rustc_cargo_argfile_compiles_and_caches() {
    let rustc = match zccache::test_support::find_rustc() {
        Some(p) => p,
        None => {
            eprintln!("skipping test: rustc not found");
            return;
        }
    };

    zccache::test_support::test_timeout(async move {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("lib.rs");
        let output = tmp.path().join("libargfile.rlib");
        // Compiles only if the quoted feature cfg and the last version cfg
        // reach rustc intact; `-D warnings` also rejects a mangled `--check-cfg`.
        let last = VERSION_CFGS - 1;
        std::fs::write(
            &src,
            format!(
                "#[cfg(not(all(feature = \"avcodec\", version_greater_than_{last})))]\n\
                 compile_error!(\"argfile cfgs did not reach rustc\");\n\
                 pub fn hello() -> i32 {{ 42 }}\n"
            ),
        )
        .unwrap();
        let argfile = write_cargo_argfile(
            tmp.path(),
            &src.to_string_lossy(),
            &output.to_string_lossy(),
        );
        let args = vec![format!("@{}", argfile.display())];

        let (endpoint, server_handle, shutdown) = start_daemon().await;
        let mut client = zccache::ipc::connect(&endpoint).await.unwrap();
        let session_id = start_session(&mut client).await;
        let rustc_str = rustc.to_string_lossy().to_string();

        let (exit_code, cached, stderr) =
            compile(&mut client, &session_id, &rustc_str, &args, tmp.path()).await;
        assert_eq!(exit_code, 0, "first compile should succeed: {stderr}");
        assert!(!cached, "first compile should be a cache miss");
        assert!(output.exists(), "output file should exist after compile");

        std::fs::remove_file(&output).unwrap();
        let (exit_code, cached, stderr) =
            compile(&mut client, &session_id, &rustc_str, &args, tmp.path()).await;
        assert_eq!(exit_code, 0, "second compile should succeed: {stderr}");
        assert!(cached, "second compile should be a cache hit");
        assert!(output.exists(), "cache hit should restore the output");

        shutdown.notify_one();
        server_handle.await.unwrap();
    })
    .await;
}
