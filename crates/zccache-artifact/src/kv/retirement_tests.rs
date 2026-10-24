//! Clear retires a namespace before later recursive cleanup.
use super::*;
use std::time::Duration;

#[test]
fn retired_namespace_sweep_preserves_a_fresh_put() {
    let directory = tempfile::tempdir().unwrap();
    let store = KvStore::open(directory.path()).unwrap();
    let key = Key::from_hex(&"a".repeat(64)).unwrap();
    store.put("exec-probe-v1", &key, b"old").unwrap();
    let detached = store
        .detach_namespace_for("exec-probe-v1", Duration::from_secs(2))
        .unwrap();
    assert!(store.get("exec-probe-v1", &key).unwrap().is_none());
    store.put("exec-probe-v1", &key, b"new").unwrap();
    let report = detached.sweep_for(Duration::from_secs(2)).unwrap();
    assert_eq!(report.failures, 0);
    assert_eq!(store.get("exec-probe-v1", &key).unwrap().unwrap(), b"new");
}

#[test]
fn dropped_retired_namespace_is_recovered_without_touching_current_values() {
    let directory = tempfile::tempdir().unwrap();
    let store = KvStore::open(directory.path()).unwrap();
    let key = Key::from_hex(&"b".repeat(64)).unwrap();
    store.put("exec-probe-v1", &key, b"old").unwrap();
    drop(
        store
            .detach_namespace_for("exec-probe-v1", Duration::from_secs(2))
            .unwrap(),
    );
    let reopened = KvStore::open(directory.path()).unwrap();
    reopened.put("exec-probe-v1", &key, b"current").unwrap();
    let recovered = reopened
        .sweep_retired_namespaces_for(Duration::from_secs(2))
        .unwrap();
    assert_eq!(recovered.failures, 0);
    assert!(recovered.reclaimed_bytes > 0);
    assert_eq!(
        reopened.get("exec-probe-v1", &key).unwrap().unwrap(),
        b"current"
    );
}

#[test]
fn namespace_retirement_survives_process_exit_and_preserves_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let output = zccache_core::wait::output_for(
        kernal_api::SpawnSpec::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "kv::retirement_tests::namespace_retirement_exit_child",
                "--ignored",
                "--nocapture",
            ])
            .env(
                "ZCCACHE_KV_RETIREMENT_EXIT_CHILD",
                directory.path().as_os_str(),
            ),
        Duration::from_secs(10),
    )
    .unwrap();
    assert_eq!(
        output.exit.raw_code(),
        86,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = KvStore::open(directory.path()).unwrap();
    let key = Key::from_hex(&"c".repeat(64)).unwrap();
    assert!(store.get("exec-probe-v1", &key).unwrap().is_none());
    store.put("exec-probe-v1", &key, b"replacement").unwrap();
    let report = store
        .sweep_retired_namespaces_for(Duration::from_secs(2))
        .unwrap();
    assert_eq!(report.failures, 0);
    assert!(!report.deferred);
    assert_eq!(
        store.get("exec-probe-v1", &key).unwrap().unwrap(),
        b"replacement"
    );
    assert!(std::fs::read_dir(directory.path().join("kv/.retired"))
        .unwrap()
        .next()
        .is_none());
}

#[test]
#[ignore = "bounded subprocess fixture, invoked by namespace_retirement_survives_process_exit"]
fn namespace_retirement_exit_child() {
    let Some(root) = std::env::var_os("ZCCACHE_KV_RETIREMENT_EXIT_CHILD") else {
        return;
    };
    let store = KvStore::open(Path::new(&root)).unwrap();
    let key = Key::from_hex(&"c".repeat(64)).unwrap();
    store.put("exec-probe-v1", &key, b"abandoned").unwrap();
    let _retired = store
        .detach_namespace_for("exec-probe-v1", Duration::from_secs(2))
        .unwrap();
    std::process::exit(86);
}
