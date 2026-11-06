//! #2091: depfile edge cases left after #1992.
//!
//! The script compiler models the depfile behaviour measured from gcc 15 and
//! clang 21: `-MD -c a.c b.c` writes `a.d` and `b.d` in the working directory,
//! `-MF` names one file that the last source overwrites, `-MF-` prints to
//! stdout, and clang ignores `-MF` without `-MD`.

use std::path::{Path, PathBuf};

use super::super::*;

/// `rust` keeps C/C++ on the legacy path; `c-cpp` selects the staged lane.
const LANES: [&str; 2] = ["rust", "c-cpp"];

struct Fixture {
    _tmp: tempfile::TempDir,
    server: DaemonServer,
    cc: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let cc = tmp.path().join("cc");
        std::fs::write(
            &cc,
            r#"#!/bin/sh
out=
dep=
md=
sources=
prev=
for arg do
    case "$prev" in
        -o) out=$arg; prev=; continue ;;
        -MF) dep=$arg; prev=; continue ;;
    esac
    case "$arg" in
        -v)
            printf '%s\n' '#include <...> search starts here:' ' /usr/include' 'End of search list.' >&2
            exit 0
            ;;
        -MD|-MMD) md=1 ;;
        -MF-) dep=- ;;
        -MF|-o) ;;
        -MF*) dep=${arg#-MF} ;;
        *.c) sources="$sources $arg" ;;
    esac
    prev=$arg
done
[ -n "$sources" ] || exit 0
for source in $sources; do
    stem=$(basename "$source" .c)
    echo "$stem" >> runs
    printf 'object' > "${out:-$stem.o}"
    [ -n "$md" ] || continue
    rule="$stem.o: $source"
    case "$dep" in
        -) printf '%s\n' "$rule" ;;
        '') printf '%s\n' "$rule" > "$stem.d" ;;
        *) printf '%s\n' "$rule" > "$dep" ;;
    esac
done
"#,
        )
        .unwrap();
        std::fs::set_permissions(&cc, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(work.join("a.c"), "int a(void) { return 1; }\n").unwrap();
        std::fs::write(work.join("b.c"), "int b(void) { return 2; }\n").unwrap();
        let server = crate::daemon::server::tests::bind_isolated_server(tmp.path());
        Self {
            _tmp: tmp,
            server,
            cc,
            work,
        }
    }

    fn runs(&self) -> usize {
        std::fs::read_to_string(self.work.join("runs")).map_or(0, |runs| runs.lines().count())
    }

    /// Remove the listed outputs as a clean rebuild does, compile, and wait
    /// for the detached store so the next compile can hit.
    async fn clean_compile(&self, args: &[&str], outputs: &[&str]) -> (bool, Vec<u8>) {
        for output in outputs {
            remove_if_present(&self.work.join(output));
        }
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        let response = handle_compile_ephemeral(
            &self.server.state,
            std::process::id(),
            &self.work,
            &self.cc,
            &args,
            &self.work,
            None,
            Vec::new(),
        )
        .await;
        let result = match response {
            Response::CompileResult {
                exit_code,
                cached,
                stdout,
                ..
            } => {
                assert_eq!(exit_code, 0);
                (cached, stdout.as_ref().clone())
            }
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
        result
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.work.join(name))
            .unwrap_or_else(|error| panic!("{name} must exist: {error}"))
    }
}

fn remove_if_present(path: &Path) {
    if path.exists() {
        crate::platform::fs::permissions::make_writable(path).unwrap();
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_source_md_leaves_every_default_depfile_on_every_compile() {
    for lane in LANES {
        let _staged = super::staged_env::StagedArtifactsEnvGuard::set(lane);
        let fx = Fixture::new();
        let args = ["-MD", "-c", "a.c", "b.c"];
        let outputs = ["a.o", "b.o", "a.d", "b.d"];
        for attempt in 0..3 {
            fx.clean_compile(&args, &outputs).await;
            assert_eq!(fx.read("a.d"), "a.o: a.c\n", "{lane} attempt {attempt}");
            assert_eq!(fx.read("b.d"), "b.o: b.c\n", "{lane} attempt {attempt}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_source_mf_leaves_the_named_depfile_on_every_compile() {
    for lane in LANES {
        let _staged = super::staged_env::StagedArtifactsEnvGuard::set(lane);
        let fx = Fixture::new();
        let args = ["-MD", "-MF", "all.d", "-c", "a.c", "b.c"];
        let outputs = ["a.o", "b.o", "all.d"];
        for attempt in 0..3 {
            fx.clean_compile(&args, &outputs).await;
            assert_eq!(fx.read("all.d"), "b.o: b.c\n", "{lane} attempt {attempt}");
        }
    }
}

/// Without `-MD` the compiler writes no depfile, so zccache must not add one
/// by injecting `-MD` next to the user's `-MF`. gcc rejects this command line
/// and that rejection must survive too.
#[tokio::test(flavor = "multi_thread")]
async fn multi_source_mf_without_md_does_not_gain_a_depfile() {
    for lane in LANES {
        let _staged = super::staged_env::StagedArtifactsEnvGuard::set(lane);
        let fx = Fixture::new();
        fx.clean_compile(&["-MF", "all.d", "-c", "a.c", "b.c"], &[])
            .await;
        assert!(!fx.work.join("all.d").exists(), "{lane}");
    }
}

/// `-MF-` sends the depfile to stdout. The staged lane must leave it there on
/// a miss and replay it on a hit, and must not create a file for it.
#[tokio::test(flavor = "multi_thread")]
async fn staged_lane_keeps_a_stdout_depfile_on_stdout() {
    for stdout_args in [&["-MF-"][..], &["-MF", "-"][..]] {
        let _staged = super::staged_env::StagedArtifactsEnvGuard::set("c-cpp");
        let fx = Fixture::new();
        let mut args = vec!["-MD"];
        args.extend_from_slice(stdout_args);
        args.extend_from_slice(&["-o", "a.o", "-c", "a.c"]);

        let (cached, stdout) = fx.clean_compile(&args, &["a.o"]).await;
        assert!(!cached);
        assert_eq!(String::from_utf8_lossy(&stdout), "a.o: a.c\n");
        assert!(!fx.work.join("a.d").exists(), "no depfile was requested");

        let (cached, stdout) = fx.clean_compile(&args, &["a.o"]).await;
        assert!(cached, "the stdout depfile compile is cacheable");
        assert_eq!(String::from_utf8_lossy(&stdout), "a.o: a.c\n");
        assert!(!fx.work.join("a.d").exists(), "no depfile was requested");
        assert_eq!(fx.runs(), 1);
    }
}

/// #2073 refused to cache this flag combination because no depfile appears,
/// but no depfile was ever requested: clang ignores `-MF` without `-MD`.
#[tokio::test(flavor = "multi_thread")]
async fn mf_without_md_caches_and_writes_no_depfile() {
    for lane in LANES {
        let _staged = super::staged_env::StagedArtifactsEnvGuard::set(lane);
        let fx = Fixture::new();
        let args = ["-MF", "ignored.d", "-o", "a.o", "-c", "a.c"];
        assert!(!fx.clean_compile(&args, &["a.o"]).await.0, "{lane}");
        assert!(fx.clean_compile(&args, &["a.o"]).await.0, "{lane}");
        assert_eq!(fx.runs(), 1, "{lane}");
        assert!(fx.work.join("a.o").exists(), "{lane}");
        assert!(!fx.work.join("ignored.d").exists(), "{lane}");
    }
}
