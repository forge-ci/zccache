//! #2103: depfile follow-ups after #2091, run against real gcc and clang.
//!
//! A compiler missing from PATH skips its cases, so a host with only one still
//! covers that one. The bare compiler is the oracle for what a depfile request
//! must leave on disk.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::super::*;
use super::staged_env::StagedArtifactsEnvGuard;

/// `rust` keeps C/C++ on the legacy path; `c-cpp` selects the staged lane.
const LANES: [&str; 2] = ["rust", "c-cpp"];

fn real_compiler(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn write_sources(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("h.h"), "#define VALUE 1\n").unwrap();
    std::fs::write(
        dir.join("a.c"),
        "#include \"h.h\"\nint a(void) { return VALUE; }\n",
    )
    .unwrap();
    std::fs::write(dir.join("b.c"), "int b(void) { return 2; }\n").unwrap();
}

struct Compile {
    exit_code: i32,
    cached: bool,
}

struct Fixture {
    tmp: tempfile::TempDir,
    server: DaemonServer,
    cc: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new(cc: &Path) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        write_sources(&work);
        let server = crate::daemon::server::tests::bind_isolated_server(tmp.path());
        Self {
            tmp,
            server,
            cc: cc.to_path_buf(),
            work,
        }
    }

    /// Remove the listed outputs as a clean rebuild does, compile, and wait
    /// for the detached store so the next compile can hit.
    async fn compile(&self, cwd: &Path, args: &[&str], outputs: &[&str]) -> Compile {
        for output in outputs {
            let path = cwd.join(output);
            if path.exists() {
                crate::platform::fs::permissions::make_writable(&path).unwrap();
                std::fs::remove_file(&path).unwrap();
            }
        }
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        let response = handle_compile_ephemeral(
            &self.server.state,
            std::process::id(),
            cwd,
            &self.cc,
            &args,
            cwd,
            None,
            Vec::new(),
        )
        .await;
        let compile = match response {
            Response::CompileResult {
                exit_code, cached, ..
            } => Compile { exit_code, cached },
            other => panic!("expected CompileResult, got {other:?}"),
        };
        let keys: Vec<String> = self
            .server
            .state
            .pending_cache_writes
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        for key in keys {
            pending_writes::await_pending_payload(
                &self.server.state.pending_cache_writes,
                &key,
                std::time::Duration::from_secs(30),
            )
            .await;
        }
        compile
    }

    /// Store what a version before #2099 stored for a legacy `-MD` batch: the
    /// object alone, under the key this batch's contexts already registered.
    fn seed_object_only_entries(&self, seed_fast_hit: bool) {
        let state = &self.server.state;
        let graph = state.dep_graph.load();
        let clock = state.cache_system.current_clock();
        let mut seeded = 0;
        for context in graph.to_snapshot().contexts {
            let source = PathBuf::from(&context.source_file);
            let context_key = crate::depgraph::context::ContextKey::from_raw(context.context_key);
            let header: NormalizedPath = self.work.join("h.h").into();
            let resolved = if source.ends_with("a.c") {
                vec![header]
            } else {
                Vec::new()
            };
            let scan = crate::depgraph::ScanResult {
                resolved,
                unresolved: Vec::new(),
                has_computed: false,
            };
            let mut tracked = scan.resolved.clone();
            tracked.push(NormalizedPath::from(source.as_path()));
            state.cache_system.register_tracked(&tracked);
            let get_hash = |path: &Path| hash_file(&state.cache_system, path, clock).ok();
            let key = graph
                .update(&context_key, scan, get_hash)
                .unwrap()
                .hash()
                .to_hex();
            let object = self
                .work
                .join(source.with_extension("o").file_name().unwrap());
            let object: NormalizedPath = object.into();
            persist_artifact_paths(
                state.artifact_dir.as_path(),
                &key,
                std::slice::from_ref(&object),
                MaterializationMode::default(),
            )
            .unwrap();
            let meta = ArtifactIndex::new(
                vec![object.file_name().unwrap().to_string_lossy().into_owned()],
                vec![std::fs::metadata(&object).unwrap().len()],
                Arc::new(Vec::new()),
                Arc::new(Vec::new()),
                0,
            );
            state
                .artifacts
                .insert(key.clone(), CachedArtifact::from_index(meta));
            if seed_fast_hit {
                // The request's key is the logical one; the snapshot's map key
                // is checkout-specific.
                state.fast_hit_cache.insert(
                    crate::depgraph::context::ContextKey::from_raw(context.logical_context_key),
                    FastHitEntry {
                        clock: state.cache_system.current_clock(),
                        artifact_key_hex: key,
                        cached_at: std::time::Instant::now(),
                    },
                );
            }
            seeded += 1;
        }
        assert_eq!(seeded, 2, "the batch registers one context per source");
    }
}

/// What the bare compiler does with the same argv in a fresh directory.
fn bare_outcome(cc: &Path, root: &Path, args: &[&str], depfile: &str) -> (bool, Option<String>) {
    let dir = root.join("bare");
    write_sources(&dir);
    let status = std::process::Command::new(cc)
        .args(args)
        .current_dir(&dir)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    (
        status.success(),
        std::fs::read_to_string(dir.join(depfile)).ok(),
    )
}

/// Every forwarded depfile spelling must behave as the bare compiler does: the
/// depfile on a miss and a hit, or the rejection on every attempt.
#[tokio::test(flavor = "multi_thread")]
async fn forwarded_depfile_flags_match_the_real_compiler() {
    let cases: [(&str, &[&str], bool); 6] = [
        ("gcc", &["-Wp,-MD,wp.d"], true),
        ("gcc", &["-Wp,-dependency-file,wp.d"], false),
        ("clang", &["-Wp,-MD,wp.d"], true),
        ("clang", &["-Wp,-MMD,wp.d"], true),
        ("clang", &["-Wp,-dependency-file,wp.d,-MT,a.o"], true),
        ("clang", &["-Wp,-dependency-file,wp.d"], false),
    ];
    let mut ran = 0;
    for (name, flags, accepted) in cases {
        let Some(cc) = real_compiler(name) else {
            eprintln!("skipping {name}: not on PATH");
            continue;
        };
        for lane in LANES {
            let _staged = StagedArtifactsEnvGuard::set(lane);
            let fx = Fixture::new(&cc);
            let mut args = flags.to_vec();
            args.extend_from_slice(&["-o", "a.o", "-c", "a.c"]);
            let label = format!("{name} {lane} {flags:?}");
            let (bare_ok, bare_depfile) = bare_outcome(&cc, fx.tmp.path(), &args, "wp.d");
            assert_eq!(bare_ok, accepted, "bare {label}");

            for attempt in 0..2 {
                let compile = fx.compile(&fx.work, &args, &["a.o", "wp.d"]).await;
                assert_eq!(compile.exit_code == 0, accepted, "{label} #{attempt}");
                assert_eq!(
                    compile.cached,
                    accepted && attempt == 1,
                    "{label} #{attempt}"
                );
                let depfile = std::fs::read_to_string(fx.work.join("wp.d")).ok();
                assert_eq!(depfile, bare_depfile, "{label} #{attempt}");
            }
            ran += 1;
        }
    }
    eprintln!("ran {ran} forwarded depfile cases");
}

/// A forwarded depfile in a batch names one file for every source. zccache
/// must leave what the bare compiler leaves, on every attempt.
#[tokio::test(flavor = "multi_thread")]
async fn forwarded_depfile_in_a_batch_matches_the_real_compiler() {
    let cases: [(&str, &[&str]); 4] = [
        ("gcc", &["-Wp,-MD,wp.d"]),
        ("gcc", &["-Wp,-dependency-file,wp.d"]),
        ("clang", &["-Wp,-MD,wp.d"]),
        ("clang", &["-Wp,-dependency-file,wp.d,-MT,out"]),
    ];
    for (name, flags) in cases {
        let Some(cc) = real_compiler(name) else {
            eprintln!("skipping {name}: not on PATH");
            continue;
        };
        for lane in LANES {
            let _staged = StagedArtifactsEnvGuard::set(lane);
            let fx = Fixture::new(&cc);
            let mut args = flags.to_vec();
            args.extend_from_slice(&["-c", "a.c", "b.c"]);
            let label = format!("{name} {lane} {flags:?}");
            let (bare_ok, bare_depfile) = bare_outcome(&cc, fx.tmp.path(), &args, "wp.d");
            for attempt in 0..3 {
                let compile = fx.compile(&fx.work, &args, &["a.o", "b.o", "wp.d"]).await;
                assert_eq!(compile.exit_code == 0, bare_ok, "{label} #{attempt}");
                let depfile = std::fs::read_to_string(fx.work.join("wp.d")).ok();
                assert_eq!(depfile, bare_depfile, "{label} #{attempt}");
            }
        }
    }
}

/// #2103 item 1: an object-only entry cannot satisfy a request for a depfile,
/// on the depgraph path or the fast-hit path. Linux and macOS never publish a
/// staged batch (no native change markers), so only a host with them can then
/// hit the replacement entry.
#[tokio::test(flavor = "multi_thread")]
async fn staged_batch_hit_without_a_depfile_payload_recompiles() {
    for name in ["gcc", "clang"] {
        let Some(cc) = real_compiler(name) else {
            eprintln!("skipping {name}: not on PATH");
            continue;
        };
        for watcher_active in [false, true] {
            let _staged = StagedArtifactsEnvGuard::set("c-cpp");
            let fx = Fixture::new(&cc);
            let args = ["-MD", "-c", "a.c", "b.c"];
            let outputs = ["a.o", "b.o", "a.d", "b.d"];
            assert!(!fx.compile(&fx.work, &args, &outputs).await.cached);

            fx.server
                .state
                .watcher_active
                .store(watcher_active, Ordering::Release);
            fx.seed_object_only_entries(watcher_active);
            let label = format!("{name} watcher_active={watcher_active}");
            let compile = fx.compile(&fx.work, &args, &outputs).await;
            assert_eq!(compile.exit_code, 0, "{label}");
            assert!(!compile.cached, "{label}: object-only entry was served");
            assert!(fx.work.join("a.d").exists(), "{label}");
            assert!(fx.work.join("b.d").exists(), "{label}");

            let compile = fx.compile(&fx.work, &args, &outputs).await;
            assert_eq!(
                compile.cached,
                super::staged_env::native_change_markers(),
                "{label}"
            );
            assert!(fx.work.join("a.d").exists(), "{label}");
        }
    }
}

/// #2103 item 2: gcc and clang write a batch's `-MD` files to the working
/// directory, so a same-named file beside the source is not the compiler's. It
/// must survive, must not supply the dependencies, and the real file must not
/// be left behind.
#[tokio::test(flavor = "multi_thread")]
async fn legacy_batch_ignores_a_user_file_beside_the_source() {
    for name in ["gcc", "clang"] {
        let Some(cc) = real_compiler(name) else {
            eprintln!("skipping {name}: not on PATH");
            continue;
        };
        let _staged = StagedArtifactsEnvGuard::set("rust");
        let fx = Fixture::new(&cc);
        let decoy = fx.work.join("a.d");
        let decoy_text = "a.o: a.c\n";
        std::fs::write(&decoy, decoy_text).unwrap();
        let build = fx.tmp.path().join("build");
        std::fs::create_dir_all(&build).unwrap();
        let args = ["-c", "../work/a.c", "../work/b.c"];
        let outputs = ["a.o", "b.o"];

        assert!(!fx.compile(&build, &args, &outputs).await.cached, "{name}");
        assert_eq!(
            std::fs::read_to_string(&decoy).unwrap(),
            decoy_text,
            "{name}"
        );
        assert!(
            !build.join("a.d").exists(),
            "{name}: injected depfile leaked"
        );
        assert!(fx.compile(&build, &args, &outputs).await.cached, "{name}");

        std::fs::write(fx.work.join("h.h"), "#define VALUE 2\n").unwrap();
        let compile = fx.compile(&build, &args, &outputs).await;
        assert!(!compile.cached, "{name}: a header edit must invalidate");
    }
}

/// #2103 item 2, the common spelling: with sources in the working directory the
/// injected `-MD` targets the user's own file, so it must not be injected.
#[tokio::test(flavor = "multi_thread")]
async fn legacy_batch_keeps_a_user_file_at_the_injected_depfile_path() {
    for name in ["gcc", "clang"] {
        let Some(cc) = real_compiler(name) else {
            eprintln!("skipping {name}: not on PATH");
            continue;
        };
        let _staged = StagedArtifactsEnvGuard::set("rust");
        let fx = Fixture::new(&cc);
        let user_file = fx.work.join("a.d");
        std::fs::write(&user_file, "module a; // a D source\n").unwrap();
        let args = ["-c", "a.c", "b.c"];
        let outputs = ["a.o", "b.o"];

        for attempt in 0..2 {
            let compile = fx.compile(&fx.work, &args, &outputs).await;
            assert_eq!(compile.exit_code, 0, "{name} #{attempt}");
            assert_eq!(
                std::fs::read_to_string(&user_file).unwrap(),
                "module a; // a D source\n",
                "{name} #{attempt}"
            );
            assert_eq!(compile.cached, attempt == 1, "{name} #{attempt}");
        }

        std::fs::write(fx.work.join("h.h"), "#define VALUE 2\n").unwrap();
        let compile = fx.compile(&fx.work, &args, &outputs).await;
        assert!(!compile.cached, "{name}: a header edit must invalidate");
    }
}
