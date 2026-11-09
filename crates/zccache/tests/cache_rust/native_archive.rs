//! Bundled native inputs must invalidate an rlib even at the same path (#1945).
#![allow(clippy::expect_used, clippy::panic)]

use super::false_green_check::{cargo, Daemon};
use std::path::{Path, PathBuf};

fn write(project: &Path, name: &str, contents: &str) {
    let path = project.join(name);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
    std::fs::write(&path, contents).expect("fixture file");
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("fixture file")
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(120))
        .expect("old fixture mtime");
}

fn outcomes(cache: &Path) -> Vec<String> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(cache).expect("cache directory") {
        let entry = entry.expect("cache entry");
        if entry.path().is_dir() {
            result.extend(outcomes(&entry.path()));
            continue;
        }
        if entry.file_name() != "compile_journal.jsonl" {
            continue;
        }
        for line in std::fs::read_to_string(entry.path())
            .expect("journal")
            .lines()
        {
            if line.contains("oraclelib") {
                let row: serde_json::Value = serde_json::from_str(line).expect("journal row");
                if row["args"].as_array().is_some_and(|args| {
                    args.windows(2)
                        .any(|pair| pair[0] == "--crate-name" && pair[1] == "oraclelib")
                }) {
                    eprintln!("native archive journal: {line}");
                    result.push(row["outcome"].as_str().expect("outcome").into());
                }
            }
        }
    }
    result
}

#[test]
#[ignore = "integration: real Soldr/Cargo and native compiler"]
fn bundled_archive_content_changes_invalidate_rlib() {
    for library in ["static=foo", "foo"] {
        archive_fixture(library);
    }
}

fn archive_fixture(library: &str) {
    let fixture = tempfile::tempdir().expect("fixture");
    let project = fixture.path().join("project");
    write(
        &project,
        "Cargo.toml",
        "[workspace]\nmembers=['oraclelib','consumer']\nresolver='2'\n",
    );
    write(
        &project,
        "rust-toolchain.toml",
        include_str!("../../../../rust-toolchain.toml"),
    );
    write(&project, "oraclelib/Cargo.toml", "[package]\nname='oraclelib'\nversion='0.0.0'\nedition='2021'\n[build-dependencies]\ncc='1'\n");
    write(
        &project,
        "oraclelib/c/foo.c",
        "int foo_answer(void) { return ANSWER; }\n",
    );
    write(&project, "oraclelib/src/lib.rs", "extern \"C\" { fn foo_answer() -> i32; }\npub fn answer() -> i32 { unsafe { foo_answer() } }\n");
    write(&project, "consumer/Cargo.toml", "[package]\nname='consumer'\nversion='0.0.0'\nedition='2021'\n[dependencies]\noraclelib={path='../oraclelib'}\n");
    write(
        &project,
        "consumer/src/main.rs",
        "fn main() { println!(\"{}\", oraclelib::answer()); }\n",
    );
    let cache = fixture.path().join("cache");
    let target = project.join("target");
    let envs = [
        ("CARGO_NET_OFFLINE", PathBuf::from("true")),
        (
            "SOLDR_RUSTC_WRAPPER",
            PathBuf::from(env!("CARGO_BIN_EXE_zccache")),
        ),
        ("ZCCACHE_CACHE_DIR", cache.clone()),
        (
            "ZCCACHE_DAEMON_NAMESPACE",
            PathBuf::from(format!("i1945-{}", std::process::id())),
        ),
    ];
    let _daemon = Daemon { envs: &envs };
    for (iteration, answer) in [7, 7, 9].into_iter().enumerate() {
        write(
            &project,
            "oraclelib/build.rs",
            &format!(
                r#"
fn main() {{
    cc::Build::new().cargo_metadata(false).file("c/foo.c").define("ANSWER", "{answer}").compile("foo");
    println!("cargo:rustc-link-search=native={{}}", std::env::var("OUT_DIR").unwrap());
    println!("cargo:rustc-link-lib={library}");
    let name = if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {{ "foo.lib" }} else {{ "libfoo.a" }};
    let archive = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join(name);
    let old = std::fs::metadata("build.rs").unwrap().modified().unwrap();
    std::fs::File::options().write(true).open(archive).unwrap().set_modified(old).unwrap();
}}
"#
            ),
        );
        let clean = cargo(&project, "clean", &target, &envs);
        assert!(clean.status.success(), "clean: {clean:?}");
        let build = cargo(&project, "build", &target, &envs);
        assert!(build.status.success(), "build: {build:?}");
        let output = std::process::Command::new(
            target
                .join("debug")
                .join(format!("consumer{}", std::env::consts::EXE_SUFFIX)),
        )
        .output()
        .expect("consumer");
        let journal = outcomes(&cache);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            answer.to_string(),
            "iteration {iteration}; journal={journal:?}"
        );
        assert_eq!(
            journal.last().map(String::as_str),
            Some(if iteration == 1 { "hit" } else { "miss" }),
            "{journal:?}"
        );
    }
}
