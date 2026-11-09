//! #1879: explicitly empty compiler roots still permit safe cache hits.
#![cfg(unix)]

use super::super::*;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

fn write_compiler(root: &Path, complete: bool) -> PathBuf {
    let compiler = root.join("fake-gcc");
    let markers = if complete {
        "echo '#include <...> search starts here:' >&2; echo 'End of search list.' >&2"
    } else {
        "echo 'broken frontend' >&2"
    };
    std::fs::write(
        &compiler,
        format!(
            r#"#!/bin/sh
out=
dep=
header=$C_INCLUDE_PATH
prev=
for arg do
    case "$prev" in
        -o) out=$arg ;;
        -MF) dep=$arg ;;
        -isystem) header=$arg ;;
    esac
    prev=$arg
    if [ "$arg" = "-v" ]; then
        echo probe >> '{}'
        {markers}
        exit 0
    fi
done
[ -n "$out" ] || exit 0
cat "$header/value.h" > "$out"
[ -z "$dep" ] || printf '%s: unit.c %s/value.h\n' "$out" "$header" > "$dep"
"#,
            root.join("probes").display()
        ),
    )
    .unwrap();
    crate::platform::fs::permissions::make_executable(&compiler).unwrap();
    compiler
}

fn write_old(path: &Path, contents: &str, age: u64) {
    std::fs::write(path, contents).unwrap();
    kernal_api::platform::fs::set_file_mtime(
        path,
        kernal_api::platform::fs::FileTime::from_system_time(
            SystemTime::now() - Duration::from_secs(age),
        ),
    )
    .unwrap();
}

async fn compile(server: &DaemonServer, root: &Path, compiler: &Path, via_env: bool) -> bool {
    let mut args = [
        "-nostdinc",
        "-c",
        "unit.c",
        "-o",
        "unit.o",
        "-MD",
        "-MF",
        "unit.d",
    ]
    .map(str::to_owned)
    .to_vec();
    let mut env: Vec<_> = std::env::vars().collect();
    if via_env {
        env.push(("C_INCLUDE_PATH".into(), root.to_string_lossy().into_owned()));
    } else {
        args.extend(["-isystem".into(), root.to_string_lossy().into_owned()]);
    }
    let response = handle_compile_ephemeral(
        &server.state,
        std::process::id(),
        root,
        compiler,
        &args,
        root,
        Some(env),
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
        other => panic!("expected compile result: {other:?}"),
    }
}

async fn check_header_tracking(via_env: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let compiler = write_compiler(root, true);
    write_old(&root.join("unit.c"), "#include <value.h>\nint value;\n", 20);
    let header = root.join("value.h");
    write_old(&header, "first header\n", 20);
    let server = super::bind_isolated_server(root);
    assert!(!compile(&server, root, &compiler, via_env).await);
    assert!(
        compile(&server, root, &compiler, via_env).await,
        "round two must hit"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("probes"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    write_old(&header, "changed header contents\n", 10);
    server.state.cache_system.apply_changes(vec![header.into()]);
    assert!(
        !compile(&server, root, &compiler, via_env).await,
        "header edit must miss"
    );
    assert_eq!(
        std::fs::read(root.join("unit.o")).unwrap(),
        b"changed header contents\n"
    );
}

#[tokio::test]
async fn known_empty_roots_track_isystem_header_edits() {
    check_header_tracking(false).await;
}

#[tokio::test]
async fn known_empty_roots_track_environment_header_edits() {
    check_header_tracking(true).await;
}

#[tokio::test]
async fn degraded_discovery_retries_and_never_hits() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let compiler = write_compiler(root, false);
    write_old(&root.join("unit.c"), "#include <value.h>\n", 20);
    write_old(&root.join("value.h"), "header\n", 20);
    let server = super::bind_isolated_server(root);
    for _ in 0..2 {
        assert!(!compile(&server, root, &compiler, false).await);
    }
    assert_eq!(
        std::fs::read_to_string(root.join("probes"))
            .unwrap()
            .lines()
            .count(),
        4
    );
}
