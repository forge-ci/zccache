//! #2027 / #1977: the PCH include alias scan is zccache's own compiler run.
//!
//! It turns the consumer's `-c` into `-M`, so a driver that adds linker
//! flags whenever it sees no `-c` (clang-tool-chain, the NixOS cc-wrapper)
//! makes clang report them as unused. The user's `-Werror` must not turn that
//! into a failed compile, and a scan that fails for any reason must fall
//! back to compiling, never to a daemon error.
//!
//! The drivers are shell wrappers around the real clang. A `--config` file
//! does not reproduce the bug on current clang: options from a config file
//! are exempt from the unused-argument diagnostic.

use super::*;

/// Write an executable `clang++` wrapper; the name keeps the Clang family.
fn write_driver(dir: &Path, clang: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let driver = dir.join("clang++");
    let script = format!(
        "#!/bin/sh\nREAL='{}'\n{body}\nexec \"$REAL\" \"$@\"\n",
        clang.display()
    );
    std::fs::write(&driver, script).unwrap();
    std::fs::set_permissions(&driver, std::fs::Permissions::from_mode(0o755)).unwrap();
    driver
}

/// Links whenever there is no `-c`, as clang-tool-chain's NixOS driver does.
const INJECT_LINKER_FLAGS: &str = "for arg in \"$@\"; do [ \"$arg\" = -c ] && exec \"$REAL\" \"$@\"; done\nexec \"$REAL\" -Wl,-rpath,/nonexistent \"$@\"";

/// Fails only zccache's alias scan.
const FAIL_ALIAS_SCAN: &str =
    "case \"$*\" in *zccache_pch_alias_scan*) echo 'forced alias scan failure' >&2; exit 1;; esac";

/// Build the PCH, record the object bare clang produces before any drift,
/// then drift the header so the consumer needs the private view.
async fn drifted_checkout(clang: &Path) -> Option<(Checkout, Daemon, ClientConn, String, Vec<u8>)> {
    let checkout = Checkout::new();
    let daemon = Daemon::start().await;
    let mut client = zccache::ipc::connect(&daemon.endpoint).await.unwrap();
    let sid = start_session(&mut client, &checkout).await;
    let include = checkout.default_include();
    build_pch(&mut client, &sid, clang, &checkout, &include, None).await;
    if !records_xxh3(&checkout, &std::fs::read(checkout.pch()).unwrap()) {
        daemon.stop().await;
        return None;
    }
    assert_consumes(&checkout, clang, "before drift");
    let object = checkout.root.join("build/use.o");
    let expected = std::fs::read(&object).unwrap();
    std::fs::remove_file(&object).unwrap();
    set_mtime(&checkout.header(), TOUCHED_SECS);
    settle();
    Some((checkout, daemon, client, sid, expected))
}

fn werror_consumer_args(checkout: &Checkout) -> Vec<String> {
    let mut args = checkout.consumer_args();
    args.push("-Werror".to_owned());
    args
}

#[tokio::test]
async fn alias_scan_ignores_driver_linker_flags_under_werror() {
    let Some(clang) = find_pch_test_clang() else {
        eprintln!("skipping test: clang not found");
        return;
    };
    let Some((checkout, daemon, mut client, sid, expected)) = drifted_checkout(&clang).await else {
        return;
    };
    let drivers = tempfile::tempdir().unwrap();
    let driver = write_driver(drivers.path(), &clang, INJECT_LINKER_FLAGS);
    let args = werror_consumer_args(&checkout);
    let (exit_code, _, stderr) =
        compile_outcome(&mut client, &sid, &driver, &checkout, args, None).await;
    assert_eq!(exit_code, 0, "consumer failed through zccache: {stderr}");
    let log = log_text(&checkout);
    assert!(log.contains("for loaded"), "private view not used:\n{log}");
    assert_eq!(
        std::fs::read(checkout.root.join("build/use.o")).unwrap(),
        expected,
        "the served compile must match the uncached object"
    );
    daemon.stop().await;
}

#[tokio::test]
async fn failed_alias_scan_falls_back_to_an_uncached_compile() {
    let Some(clang) = find_pch_test_clang() else {
        eprintln!("skipping test: clang not found");
        return;
    };
    let Some((checkout, daemon, mut client, sid, expected)) = drifted_checkout(&clang).await else {
        return;
    };
    let drivers = tempfile::tempdir().unwrap();
    let driver = write_driver(drivers.path(), &clang, FAIL_ALIAS_SCAN);
    let args = werror_consumer_args(&checkout);
    let (exit_code, cached, stderr) =
        compile_outcome(&mut client, &sid, &driver, &checkout, args, None).await;
    assert_eq!(
        exit_code, 0,
        "a failed internal scan failed the compile: {stderr}"
    );
    assert!(
        !cached,
        "a compile without its alias scan must not be cached"
    );
    assert!(
        !stderr.contains("PCH alias"),
        "scan failure leaked to the client: {stderr}"
    );
    assert_eq!(
        std::fs::read(checkout.root.join("build/use.o")).unwrap(),
        expected
    );
    daemon.stop().await;
}
