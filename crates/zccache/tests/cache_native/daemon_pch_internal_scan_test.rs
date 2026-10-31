//! #2027 / #1977: the PCH include alias scan is zccache's own compiler run.
//!
//! It turns the consumer's `-c` into `-M`, so a driver that adds linker
//! flags whenever it sees no `-c` (clang-tool-chain, the NixOS cc-wrapper)
//! makes clang report them as unused. The user's `-Werror` must not turn that
//! into a failed compile, and a scan that fails for any reason must fall
//! back to compiling, never to a daemon error.
//!
//! The wrapper driver below is the reproducer: it adds the flag on the
//! command line, as clang-tool-chain and the NixOS cc-wrapper do, and fails
//! on pre-#2028 daemons.
//!
//! #2027 also asks for a clang *config file* that injects the flag, through
//! both `--config=<file>` and a `<driver>.cfg` beside a symlinked clang.
//! Those tests are below, but they cannot fail on any shipped clang: clang
//! 18.1.8, 19.1.7, 20.1.8 and 21.1.8 never report config-file options as
//! unused, whatever the ordering (`-Werror` before or after `--config`,
//! `-Werror=unused-command-line-argument`, `-Werror` inside the file,
//! `-Wl,` / `-Xlinker` / `-L`+`-l` in the file), for `-M` and `-c` alike.
//! They pin the contract that a config-file toolchain still gets a served,
//! byte-identical `-Werror` compile through the scan.

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

/// A driver symlink named `clang++` with `clang++.cfg` beside it. Clang
/// resolves the driver's real path before it searches its own directory
/// (clang-tool-chain then finds its bundled `clang++.cfg` instead), so the
/// symlink's directory is named as the user config directory, which clang
/// searches first for the default `<driver>.cfg`.
fn driver_with_adjacent_config(dir: &Path, clang: &Path) -> (PathBuf, Vec<String>) {
    let driver = dir.join("clang++");
    std::os::unix::fs::symlink(clang, &driver).unwrap();
    std::fs::write(dir.join("clang++.cfg"), "-Wl,-rpath,/nonexistent\n").unwrap();
    (driver, vec![format!("--config-user-dir={}", dir.display())])
}

/// Compile the consumer through the daemon with `driver` and `extra` args,
/// and compare it with the object the same driver and args produce
/// uncached, before the header drifts.
async fn config_file_consumer_matches_uncached(driver: &Path, extra: &[String]) {
    let Some(clang) = find_pch_test_clang() else {
        eprintln!("skipping test: clang not found");
        return;
    };
    let checkout = Checkout::new();
    let daemon = Daemon::start().await;
    let mut client = zccache::ipc::connect(&daemon.endpoint).await.unwrap();
    let sid = start_session(&mut client, &checkout).await;
    let include = checkout.default_include();
    build_pch(&mut client, &sid, &clang, &checkout, &include, None).await;
    if !records_xxh3(&checkout, &std::fs::read(checkout.pch()).unwrap()) {
        daemon.stop().await;
        return;
    }
    let mut args = werror_consumer_args(&checkout);
    args.extend_from_slice(extra);
    let object = checkout.root.join("build/use.o");
    let bare = std::process::Command::new(driver)
        .current_dir(checkout.root.join("build"))
        .args(&args)
        .output()
        .unwrap();
    assert!(
        bare.status.success(),
        "uncached reference compile failed: {}",
        String::from_utf8_lossy(&bare.stderr)
    );
    // The fixture is only meaningful if clang actually loaded the config.
    let link_plan = std::process::Command::new(driver)
        .args(extra)
        .args(["-###", "-o", "use"])
        .arg(&object)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&link_plan.stderr).contains("/nonexistent"),
        "clang did not load the config file: {}",
        String::from_utf8_lossy(&link_plan.stderr)
    );
    let expected = std::fs::read(&object).unwrap();
    std::fs::remove_file(&object).unwrap();
    set_mtime(&checkout.header(), TOUCHED_SECS);
    settle();
    let (exit_code, _, stderr) =
        compile_outcome(&mut client, &sid, driver, &checkout, args, None).await;
    assert_eq!(exit_code, 0, "consumer failed through zccache: {stderr}");
    let log = log_text(&checkout);
    assert!(log.contains("for loaded"), "private view not used:\n{log}");
    assert_eq!(
        std::fs::read(&object).unwrap(),
        expected,
        "the served compile must match the uncached object"
    );
    daemon.stop().await;
}

#[tokio::test]
async fn alias_scan_under_werror_with_command_line_config_file() {
    let Some(clang) = find_pch_test_clang() else {
        eprintln!("skipping test: clang not found");
        return;
    };
    let configs = tempfile::tempdir().unwrap();
    let config = configs.path().join("inject.cfg");
    std::fs::write(&config, "-Wl,-rpath,/nonexistent\n").unwrap();
    let extra = [format!("--config={}", config.display())];
    config_file_consumer_matches_uncached(&clang, &extra).await;
}

#[tokio::test]
async fn alias_scan_under_werror_with_driver_adjacent_config_file() {
    let Some(clang) = find_pch_test_clang() else {
        eprintln!("skipping test: clang not found");
        return;
    };
    let drivers = tempfile::tempdir().unwrap();
    let (driver, extra) = driver_with_adjacent_config(drivers.path(), &clang);
    config_file_consumer_matches_uncached(&driver, &extra).await;
}
