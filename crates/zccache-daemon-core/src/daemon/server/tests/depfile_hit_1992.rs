//! #1992: a C/C++ hit must never leave a requested depfile missing or empty.
//!
//! Ninja deletes each depfile after ingesting it and records `#deps 0` for a
//! missing or empty one, so the object silently never rebuilds on a header
//! edit. These compiles run through a script compiler on real files.

use std::path::{Path, PathBuf};

use super::super::*;

struct Fixture {
    _tmp: tempfile::TempDir,
    server: DaemonServer,
    cc: PathBuf,
    work: PathBuf,
    args: Vec<String>,
}

impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let cc = tmp.path().join("cc");
        // `empty-depfile` in the working directory makes the compiler write a
        // zero-byte depfile. Every real compile appends to `runs`.
        std::fs::write(
            &cc,
            r#"#!/bin/sh
out=
dep=
prev=
for arg do
    case "$prev" in
        -o) out=$arg ;;
        -MF) dep=$arg ;;
    esac
    prev=$arg
    if [ "$arg" = "-v" ]; then
        printf '%s\n' '#include <...> search starts here:' ' /usr/include' 'End of search list.' >&2
        exit 0
    fi
done
[ -n "$out" ] || exit 0
echo run >> runs
printf 'object' > "$out"
if [ -e empty-depfile ]; then
    : > "$dep"
else
    printf 'unit.o: unit.c\n' > "$dep"
fi
"#,
        )
        .unwrap();
        std::fs::set_permissions(&cc, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(work.join("unit.c"), "int unit(void) { return 1; }\n").unwrap();
        let args = [
            "-MD", "-MT", "unit.o", "-MF", "unit.o.d", "-o", "unit.o", "-c", "unit.c",
        ]
        .map(str::to_string)
        .to_vec();
        let server = crate::daemon::server::tests::bind_isolated_server(tmp.path());
        Self {
            _tmp: tmp,
            server,
            cc,
            work,
            args,
        }
    }

    fn runs(&self) -> usize {
        std::fs::read_to_string(self.work.join("runs")).map_or(0, |runs| runs.lines().count())
    }

    fn depfile(&self) -> PathBuf {
        self.work.join("unit.o.d")
    }

    /// Delete the outputs as Ninja and a clean rebuild do, then compile.
    async fn clean_compile(&self) -> bool {
        remove_if_present(&self.depfile());
        remove_if_present(&self.work.join("unit.o"));
        let response = handle_compile_ephemeral(
            &self.server.state,
            std::process::id(),
            &self.work,
            &self.cc,
            &self.args,
            &self.work,
            None,
            Vec::new(),
        )
        .await;
        match response {
            Response::CompileResult {
                exit_code, cached, ..
            } => {
                assert_eq!(exit_code, 0);
                cached
            }
            other => panic!("expected CompileResult, got {other:?}"),
        }
    }

    /// Wait for the detached store of the previous miss.
    async fn settle(&self) {
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
    }
}

fn remove_if_present(path: &Path) {
    if path.exists() {
        crate::platform::fs::permissions::make_writable(path).unwrap();
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_compiler_depfile_is_never_stored_or_replayed() {
    let fx = Fixture::new();
    std::fs::write(fx.work.join("empty-depfile"), "").unwrap();
    assert!(!fx.clean_compile().await);
    fx.settle().await;
    assert!(
        fx.server.state.artifacts.is_empty(),
        "an entry without a usable depfile must not be published"
    );

    assert!(!fx.clean_compile().await, "nothing was stored to hit");
    assert_eq!(fx.runs(), 2);

    std::fs::remove_file(fx.work.join("empty-depfile")).unwrap();
    assert!(!fx.clean_compile().await);
    fx.settle().await;
    assert!(fx.clean_compile().await, "a good depfile caches normally");
    assert_eq!(fx.runs(), 3);
    assert_eq!(
        std::fs::read_to_string(fx.depfile()).unwrap(),
        "unit.o: unit.c\n"
    );
}

/// An entry an older zccache persisted without its depfile recompiles, and the
/// recompile replaces it so later hits restore the depfile.
#[tokio::test(flavor = "multi_thread")]
async fn a_persisted_entry_without_a_depfile_is_a_miss_that_heals() {
    let fx = Fixture::new();
    assert!(!fx.clean_compile().await);
    fx.settle().await;
    let state = &fx.server.state;
    let key = state.artifacts.iter().next().unwrap().key().clone();
    let bad = state.artifact_dir.join("bad-object");
    std::fs::write(&bad, "object").unwrap();
    write_authoritative_blob_digest(&bad).unwrap();
    state.artifacts.insert(
        key,
        CachedArtifact::from_file_payloads(
            ArtifactIndex::new(
                vec!["unit.o".to_string()],
                vec![6],
                Arc::new(Vec::new()),
                Arc::new(Vec::new()),
                0,
            ),
            vec![bad],
        ),
    );

    assert!(
        !fx.clean_compile().await,
        "the bad entry must not be served"
    );
    assert_eq!(fx.runs(), 2);
    assert_eq!(
        std::fs::read_to_string(fx.depfile()).unwrap(),
        "unit.o: unit.c\n"
    );
    fx.settle().await;
    assert!(fx.clean_compile().await, "the recompile healed the entry");
    assert_eq!(fx.runs(), 2);
    assert_eq!(
        std::fs::read_to_string(fx.depfile()).unwrap(),
        "unit.o: unit.c\n"
    );
}
