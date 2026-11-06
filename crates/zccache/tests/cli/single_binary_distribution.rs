//! Distribution contract for #1000: the shipped `zccache` executable hosts
//! both daemon entry points under exact argv[0] names.

use std::process::Command;

#[test]
fn zccache_copy_dispatches_as_download_daemon() {
    let source = std::path::Path::new(env!("CARGO_BIN_EXE_zccache"));
    let temp = tempfile::tempdir().expect("create isolated deploy directory");
    let deployed = temp.path().join(if cfg!(windows) {
        "zccache-download-daemon.exe"
    } else {
        "zccache-download-daemon"
    });
    std::fs::copy(source, &deployed).expect("copy multicall binary under download-daemon name");

    let output = Command::new(&deployed)
        .arg("--version")
        .output()
        .expect("run self-deployed download daemon copy");

    assert!(
        output.status.success(),
        "download-daemon dispatch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).starts_with("zccache-download-daemon "),
        "argv[0] did not select the download-daemon entry: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// #2090, with the shipped binary: a custom-endpoint daemon records itself under
/// its own endpoint, and a client whose endpoint has vanished replaces it.
#[cfg(unix)]
#[test]
fn an_unreachable_download_daemon_on_a_custom_endpoint_is_replaced() {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    const BUDGET: Duration = Duration::from_secs(30);

    fn lock_files(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                lock_files(&path, found);
            } else if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("download-daemon"))
                && path.extension().is_some_and(|ext| ext == "lock")
            {
                found.push(path);
            }
        }
    }

    fn scoped_lock_pid(dir: &Path) -> Option<u32> {
        let mut found = Vec::new();
        lock_files(dir, &mut found);
        assert!(
            found
                .iter()
                .all(|path| !path.ends_with("download-daemon.lock")),
            "a custom-endpoint daemon wrote the default lock: {found:?}"
        );
        std::fs::read_to_string(found.first()?)
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let cache = temp.path().join("cache");
    let endpoint = temp.path().join("dl.sock");
    let deployed = temp.path().join("zccache-download-daemon");
    std::fs::copy(env!("CARGO_BIN_EXE_zccache"), &deployed).expect("deploy");

    let mut daemon = Command::new(&deployed)
        .args(["--foreground", "--endpoint"])
        .arg(&endpoint)
        .env("ZCCACHE_CACHE_DIR", &cache)
        .env_remove("ZCCACHE_DAEMON_NAMESPACE")
        .spawn()
        .expect("spawn daemon");
    let deadline = Instant::now() + BUDGET;
    while scoped_lock_pid(&cache) != Some(daemon.id()) {
        assert!(Instant::now() < deadline, "daemon never recorded itself");
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(endpoint.exists(), "the lock must follow the bind");

    std::fs::remove_file(&endpoint).expect("take the endpoint away");
    let client = Command::new(env!("CARGO_BIN_EXE_zccache"))
        .args(["download", "--url", "http://127.0.0.1:1/absent"])
        .arg(temp.path().join("out"))
        .env("ZCCACHE_CACHE_DIR", &cache)
        .env("ZCCACHE_DOWNLOAD_ENDPOINT", &endpoint)
        .env_remove("ZCCACHE_DAEMON_NAMESPACE")
        .output()
        .expect("run client");
    let stderr = String::from_utf8_lossy(&client.stderr);

    let replacement = scoped_lock_pid(&cache);
    if let Some(pid) = replacement {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
    let deadline = Instant::now() + BUDGET;
    let old_exited = loop {
        if daemon.try_wait().expect("poll daemon").is_some() {
            break true;
        }
        if Instant::now() >= deadline {
            let _ = daemon.kill();
            let _ = daemon.wait();
            break false;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(old_exited, "the proven instance must be stopped: {stderr}");
    assert!(
        replacement.is_some_and(|pid| pid != daemon.id()),
        "a replacement must hold the lock: {stderr}"
    );
    assert!(!stderr.contains("not accepting connections"), "{stderr}");
}

/// #2106, with real processes: a second daemon on a live endpoint exits without
/// orphaning the first, and a SIGKILLed daemon's socket is reclaimed.
#[cfg(unix)]
#[test]
fn a_second_download_daemon_cannot_orphan_a_live_one_and_a_crash_is_reclaimed() {
    use std::path::Path;
    use std::process::Child;
    use std::time::{Duration, Instant};

    const BUDGET: Duration = Duration::from_secs(30);

    fn serving(deployed: &Path, endpoint: &Path, cache: &Path) -> bool {
        let output = Command::new(deployed)
            .arg("--endpoint")
            .arg(endpoint)
            .env("ZCCACHE_CACHE_DIR", cache)
            .output()
            .expect("query status");
        String::from_utf8_lossy(&output.stdout).contains("status:     running")
    }

    struct Daemon(Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let cache = temp.path().join("cache");
    let endpoint = temp.path().join("dl.sock");
    let deployed = temp.path().join("zccache-download-daemon");
    std::fs::copy(env!("CARGO_BIN_EXE_zccache"), &deployed).expect("deploy");
    let spawn = || {
        Daemon(
            Command::new(&deployed)
                .args(["--foreground", "--endpoint"])
                .arg(&endpoint)
                .env("ZCCACHE_CACHE_DIR", &cache)
                .spawn()
                .expect("spawn daemon"),
        )
    };
    let wait_serving = || {
        let deadline = Instant::now() + BUDGET;
        while !serving(&deployed, &endpoint, &cache) {
            assert!(Instant::now() < deadline, "daemon never served");
            std::thread::sleep(Duration::from_millis(25));
        }
    };

    let mut first = spawn();
    wait_serving();
    let inode = |path: &Path| {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(path).expect("socket").ino()
    };
    let first_socket = inode(&endpoint);

    let mut second = spawn();
    let deadline = Instant::now() + BUDGET;
    let status = loop {
        if let Some(status) = second.0.try_wait().expect("poll second") {
            break status;
        }
        assert!(Instant::now() < deadline, "the second daemon kept running");
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(!status.success(), "the loser must report the failed bind");
    assert_eq!(inode(&endpoint), first_socket, "the socket was replaced");
    assert!(first.0.try_wait().expect("poll first").is_none());
    assert!(serving(&deployed, &endpoint, &cache), "first was orphaned");

    first.0.kill().expect("SIGKILL");
    first.0.wait().expect("reap");
    assert!(endpoint.exists(), "the crash must leave a stale socket");
    let _third = spawn();
    wait_serving();
}
