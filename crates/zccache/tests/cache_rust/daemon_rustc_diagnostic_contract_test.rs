//! Replayed rustc diagnostics honor the requesting invocation's rendering
//! flags (#1929). Same source, different `--error-format`, `--json`, `--color`
//! or `--diagnostic-width`: artifact bytes may be shared, stderr may not.
//!
//! The negative half matters as much as the positive half: flags that cannot
//! change the rendered bytes must keep hitting, or every key split costs a
//! recompile. `rustc_ground_truth_*` pins those equivalences on the real
//! compiler, so a toolchain bump that breaks one fails here first.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unwrap_in_result
)]

use std::path::{Path, PathBuf};

use zccache::core::NormalizedPath;
use zccache::daemon::DaemonServer;
use zccache::protocol::{Request, Response};

#[cfg(unix)]
type ClientConn = zccache::ipc::IpcConnection;
#[cfg(windows)]
type ClientConn = zccache::ipc::IpcClientConnection;

/// A warning whose source line overflows a narrow width, so width changes the
/// rendered text rather than being vacuously equal.
const WARNING_SOURCE: &str = "pub fn f() { let unused_variable_with_a_really_long_name_for_wrapping_abcdefghijklmnopqrstuvwxyz_0123456789 = 1; /* long trailing comment that overflows narrow terminals */ }\n";
const CLEAN_SOURCE: &str = "pub fn clean() {}\n";
const ERROR_SOURCE: &str = "pub fn broken() -> u32 { \"not a number\" }\n";
/// Cargo's constant selection, captured from its real rustc argv.
const CARGO_JSON: &str = "--json=diagnostic-rendered-ansi,artifacts,future-incompat";

/// The toolchain's own `rustc`. `find_rustc` can return a soldr shim on a
/// developer host, and that shim replays from its own daemon cache, which is
/// the very behavior under test.
fn real_rustc() -> Option<PathBuf> {
    let shim = zccache::test_support::find_rustc()?;
    let output = std::process::Command::new(shim.as_path())
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    let sysroot = String::from_utf8(output.stdout).ok()?;
    let rustc = Path::new(sysroot.trim())
        .join("bin")
        .join(format!("rustc{}", std::env::consts::EXE_SUFFIX));
    rustc.is_file().then_some(rustc)
}

struct Outcome {
    exit_code: i32,
    cached: bool,
    stderr: String,
}

struct Harness {
    client: ClientConn,
    session_id: String,
    rustc: String,
    dir: tempfile::TempDir,
    shutdown: std::sync::Arc<kernal_api::async_engine::Notify>,
    server: tokio::task::JoinHandle<()>,
}

impl Harness {
    async fn start(rustc: &Path, source: &str) -> Self {
        let endpoint = zccache::ipc::unique_test_endpoint();
        let cache_root = tempfile::tempdir().unwrap();
        let cache_dir: NormalizedPath = cache_root.path().join("zccache-cache").into();
        let mut server = DaemonServer::bind_with_cache_dir(&endpoint, &cache_dir).unwrap();
        let shutdown = server.shutdown_handle();
        let server = tokio::spawn(async move {
            let _cache_root = cache_root;
            server.run(0).await.unwrap();
        });
        let mut client = zccache::ipc::connect(&endpoint).await.unwrap();
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
        let session_id = match client.recv().await.unwrap() {
            Some(Response::SessionStarted { session_id, .. }) => session_id,
            other => panic!("expected SessionStarted, got: {other:?}"),
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), source).unwrap();
        Self {
            client,
            session_id,
            rustc: rustc.to_string_lossy().into_owned(),
            dir,
            shutdown,
            server,
        }
    }

    /// One cargo-check-shaped compile with `rendering` flags appended.
    async fn compile(&mut self, rendering: &[&str]) -> Outcome {
        let mut args: Vec<String> = [
            "--crate-name=contract",
            "--crate-type=lib",
            "--edition=2021",
            "--emit=dep-info,metadata",
            "--out-dir=out",
            "lib.rs",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        args.extend(rendering.iter().map(ToString::to_string));
        self.client
            .send(&Request::Compile {
                session_id: self.session_id.clone(),
                args,
                cwd: self.dir.path().to_path_buf().into(),
                compiler: NormalizedPath::new(&self.rustc),
                env: None,
                stdin: Vec::new(),
            })
            .await
            .unwrap();
        loop {
            match self.client.recv().await.unwrap() {
                Some(Response::CompileProgress { .. }) => continue,
                Some(Response::CompileResult {
                    exit_code,
                    cached,
                    stderr,
                    ..
                }) => {
                    return Outcome {
                        exit_code,
                        cached,
                        stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    }
                }
                other => panic!("unexpected response: {other:?}"),
            }
        }
    }

    async fn stop(self) {
        self.shutdown.notify_one();
        drop(self.client);
        self.server.await.unwrap();
    }
}

/// Every non-empty line is a JSON object, and at least one is a diagnostic.
fn assert_json_diagnostics(stderr: &str) {
    let mut diagnostics = 0;
    for line in stderr.lines().filter(|line| !line.is_empty()) {
        let value: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("not JSON ({error}): {line}\nfull stderr:\n{stderr}"));
        if value["$message_type"] == "diagnostic" {
            diagnostics += 1;
        }
    }
    assert!(diagnostics > 0, "no JSON diagnostic in stderr:\n{stderr}");
}

fn assert_human_diagnostics(stderr: &str) {
    assert!(
        stderr.contains("warning: unused variable") || stderr.contains("error[E0308]"),
        "expected human diagnostics:\n{stderr}"
    );
    assert!(
        !stderr.contains("\"$message_type\""),
        "human request was served JSON:\n{stderr}"
    );
}

macro_rules! require_rustc {
    () => {
        match real_rustc() {
            Some(rustc) => rustc,
            None => return,
        }
    };
}

/// The issue's acceptance case, in both directions, and both contracts keep
/// hitting afterward rather than evicting one another.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warning_is_replayed_in_the_requested_format() {
    let rustc = require_rustc!();
    zccache::test_support::test_timeout(async move {
        let mut h = Harness::start(&rustc, WARNING_SOURCE).await;
        let json = ["--error-format=json", CARGO_JSON];

        let human_miss = h.compile(&[]).await;
        assert_eq!(human_miss.exit_code, 0);
        assert!(!human_miss.cached);
        assert_human_diagnostics(&human_miss.stderr);

        let json_miss = h.compile(&json).await;
        assert_eq!(json_miss.exit_code, 0);
        assert!(!json_miss.cached, "must not replay another format's stderr");
        assert_json_diagnostics(&json_miss.stderr);

        let human_hit = h.compile(&[]).await;
        assert!(
            human_hit.cached,
            "human verdict must survive the JSON store"
        );
        assert_eq!(human_hit.stderr, human_miss.stderr);

        let json_hit = h.compile(&json).await;
        assert!(json_hit.cached, "JSON verdict must be reusable");
        assert_json_diagnostics(&json_hit.stderr);
        h.stop().await;
    })
    .await;
}

/// Cached compiler errors are replayed through the same verdict map.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_error_is_replayed_in_the_requested_format() {
    let rustc = require_rustc!();
    zccache::test_support::test_timeout(async move {
        let mut h = Harness::start(&rustc, ERROR_SOURCE).await;
        // Errors are cached only beside an existing dep-info file.
        std::fs::create_dir_all(h.dir.path().join("out")).unwrap();
        std::fs::write(h.dir.path().join("out/contract.d"), "").unwrap();
        let json = ["--error-format=json", CARGO_JSON];

        let human = h.compile(&[]).await;
        assert_ne!(human.exit_code, 0);
        assert_human_diagnostics(&human.stderr);
        let replayed = h.compile(&[]).await;
        assert_ne!(replayed.exit_code, 0);
        assert!(replayed.cached, "the error must really be cached");

        let as_json = h.compile(&json).await;
        assert_ne!(as_json.exit_code, 0);
        assert!(!as_json.cached, "must not replay another format's error");
        assert_json_diagnostics(&as_json.stderr);

        let human_again = h.compile(&[]).await;
        assert_ne!(human_again.exit_code, 0);
        assert!(human_again.cached, "both error contracts must coexist");
        assert_human_diagnostics(&human_again.stderr);
        h.stop().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_selection_and_color_and_width_keep_their_own_replay() {
    let rustc = require_rustc!();
    zccache::test_support::test_timeout(async move {
        let mut h = Harness::start(&rustc, WARNING_SOURCE).await;
        let plain = h.compile(&["--error-format=json"]).await;
        assert_json_diagnostics(&plain.stderr);

        // ANSI selection changes the JSON `rendered` field.
        let ansi = h
            .compile(&["--error-format=json", "--json=diagnostic-rendered-ansi"])
            .await;
        assert!(!ansi.cached, "--json selection must not share a replay");
        assert!(ansi.stderr.contains("\\u001b["), "{}", ansi.stderr);

        // Human ANSI color.
        let human = h.compile(&[]).await;
        assert!(!human.stderr.contains('\u{1b}'), "{}", human.stderr);
        let color = h.compile(&["--color=always"]).await;
        assert!(!color.cached, "--color must not share a human replay");
        assert!(color.stderr.contains('\u{1b}'), "{}", color.stderr);

        // Width over a diagnostic, compared with the unset default.
        let narrow = h.compile(&["--diagnostic-width=40"]).await;
        assert!(!narrow.cached, "width must not share a rendered diagnostic");
        assert_ne!(narrow.stderr, human.stderr);
        h.stop().await;
    })
    .await;
}

/// Formats with identical output bytes share one artifact, whose verdict map
/// must hold both: a publication conflict here would evict the artifact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_artifact_holds_a_verdict_per_format() {
    let rustc = require_rustc!();
    zccache::test_support::test_timeout(async move {
        let mut h = Harness::start(&rustc, WARNING_SOURCE).await;
        let json = ["--error-format=json"];
        let human = h.compile(&[]).await;
        let as_json = h.compile(&json).await;
        assert!(!as_json.cached);
        assert_json_diagnostics(&as_json.stderr);
        for _ in 0..2 {
            let human_hit = h.compile(&[]).await;
            assert!(human_hit.cached, "human verdict must survive");
            assert_eq!(human_hit.stderr, human.stderr);
            let json_hit = h.compile(&json).await;
            assert!(json_hit.cached, "JSON verdict must survive");
            assert_eq!(json_hit.stderr, as_json.stderr);
        }
        h.stop().await;
    })
    .await;
}

/// An invalid flag pair must reach rustc rather than hit a valid verdict.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn color_beside_json_still_gets_rustcs_error() {
    let rustc = require_rustc!();
    zccache::test_support::test_timeout(async move {
        let mut h = Harness::start(&rustc, WARNING_SOURCE).await;
        assert_eq!(
            h.compile(&["--error-format=json", CARGO_JSON])
                .await
                .exit_code,
            0
        );
        let invalid = h
            .compile(&["--error-format=json", CARGO_JSON, "--color=always"])
            .await;
        assert_ne!(invalid.exit_code, 0, "{}", invalid.stderr);
        assert!(invalid.stderr.contains("--color"), "{}", invalid.stderr);
        h.stop().await;
    })
    .await;
}

/// Spellings rustc treats identically must not split the verdict.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn equivalent_spellings_share_one_verdict() {
    let rustc = require_rustc!();
    zccache::test_support::test_timeout(async move {
        let mut h = Harness::start(&rustc, WARNING_SOURCE).await;
        let first = h.compile(&["--error-format=json", CARGO_JSON]).await;
        assert!(!first.cached);
        for equivalent in [
            vec![
                "--error-format=json",
                "--json=future-incompat,artifacts,diagnostic-rendered-ansi",
            ],
            vec![
                "--error-format=json",
                "--json=artifacts",
                "--json=diagnostic-rendered-ansi,future-incompat",
            ],
        ] {
            let again = h.compile(&equivalent).await;
            assert!(again.cached, "{equivalent:?} must hit");
        }

        let human = h.compile(&[]).await;
        assert!(!human.cached);
        for equivalent in [vec!["--error-format=human"], vec!["--color=auto"]] {
            let again = h.compile(&equivalent).await;
            assert!(again.cached, "{equivalent:?} must hit");
            assert_eq!(again.stderr, human.stderr);
        }
        h.stop().await;
    })
    .await;
}

/// Cargo on a terminal passes its width; without one it passes nothing. Width
/// has nothing to render on a clean compile, so a resize must not recompile.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_compile_does_not_split_on_width() {
    let rustc = require_rustc!();
    zccache::test_support::test_timeout(async move {
        let mut h = Harness::start(&rustc, CLEAN_SOURCE).await;
        let json = ["--error-format=json", CARGO_JSON];
        assert!(!h.compile(&json).await.cached);
        for width in ["--diagnostic-width=80", "--diagnostic-width=200"] {
            let again = h.compile(&["--error-format=json", CARGO_JSON, width]).await;
            assert!(again.cached, "{width} must hit a clean verdict");
        }
        h.stop().await;
    })
    .await;
}

/// The reverse order: a clean verdict recorded under a width is just as
/// reusable with no width at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_verdict_recorded_with_width_serves_unset_width() {
    let rustc = require_rustc!();
    zccache::test_support::test_timeout(async move {
        let mut h = Harness::start(&rustc, CLEAN_SOURCE).await;
        assert!(!h.compile(&["--diagnostic-width=80"]).await.cached);
        assert!(h.compile(&[]).await.cached);
        h.stop().await;
    })
    .await;
}

/// Runs the real compiler with no cache, for the equivalences keys rely on.
fn rustc_stderr(rustc: &Path, dir: &Path, flags: &[&str]) -> String {
    let output = std::process::Command::new(rustc)
        .current_dir(dir)
        .args([
            "--crate-type=lib",
            "--emit=metadata",
            "--out-dir=o",
            "lib.rs",
        ])
        .args(flags)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Output bytes under `flags`, which decides verdict versus context keying.
fn rustc_rmeta(rustc: &Path, dir: &Path, flags: &[&str]) -> Vec<u8> {
    rustc_stderr(rustc, dir, flags);
    std::fs::read(dir.join("o/liblib.rmeta")).unwrap()
}

/// `--json` is tracked by rustc and changes output bytes; the other rendering
/// flags do not. Reusing one artifact key across differing bytes is a
/// publication conflict that evicts the artifact.
#[test]
fn rustc_ground_truth_for_output_bytes() {
    let rustc = require_rustc!();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), WARNING_SOURCE).unwrap();
    let bytes = |flags: &[&str]| rustc_rmeta(&rustc, dir.path(), flags);
    let base = bytes(&[]);
    for untracked in [
        &["--error-format=json"][..],
        &["--error-format=short"][..],
        &["--color=always"][..],
        &["--diagnostic-width=40"][..],
    ] {
        assert!(
            base == bytes(untracked),
            "{untracked:?} changed output bytes"
        );
    }
    assert!(base != bytes(&["--error-format=json", CARGO_JSON]));
}

#[test]
fn rustc_ground_truth_for_the_rendering_contract() {
    let rustc = require_rustc!();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), WARNING_SOURCE).unwrap();
    let run = |flags: &[&str]| rustc_stderr(&rustc, dir.path(), flags);
    let json = ["--error-format=json", "--json=diagnostic-rendered-ansi"];

    // rustc rejects --color beside --json, so the key must not fold it away:
    // doing so would answer an invalid command line with a cache hit.
    for color in ["--color=always", "--color=never"] {
        let flags = [json[0], json[1], color];
        assert!(run(&flags).contains("cannot specify the `--color` option"));
    }
    // Width changes rendered text in every format that renders.
    assert_ne!(run(&[]), run(&["--diagnostic-width=40"]));
    assert_ne!(
        run(&["--error-format=json"]),
        run(&["--error-format=json", "--diagnostic-width=40"])
    );
    // --color=auto is the default.
    assert_eq!(run(&[]), run(&["--color=auto"]));
    assert_eq!(run(&[]), run(&["--error-format=human"]));
    // --json repeats and accumulates, in any order.
    assert_eq!(
        run(&["--error-format=json", "--json=artifacts,diagnostic-short"]),
        run(&[
            "--error-format=json",
            "--json=diagnostic-short",
            "--json=artifacts"
        ])
    );
    // Selectors are not cosmetic: `artifacts` adds notification lines.
    assert_ne!(
        run(&["--error-format=json"]),
        run(&["--error-format=json", "--json=artifacts"])
    );
}
