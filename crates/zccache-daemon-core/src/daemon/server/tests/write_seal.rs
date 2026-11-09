//! #1791: seal upgrades preserve rustc replacement and #1039 write protection.

use super::super::*;
use crate::platform::fs::permissions::{
    is_sealed, make_writable, seal_cache_blob, set_cache_blob_sealed, set_readonly,
};
use std::time::{Duration, Instant};

fn assert_write_denied(path: &Path, root: &Path) {
    assert!(is_sealed(path).unwrap());
    assert!(!std::fs::metadata(path).unwrap().permissions().readonly());
    if !super::wrapperless_rustc_rebuild::privileged(root) {
        assert!(std::fs::OpenOptions::new().append(true).open(path).is_err());
    }
}

#[test]
fn write_seal_legacy_hardlink_hit_upgrades_and_stays_replaceable() {
    let dir = tempfile::tempdir().unwrap();
    let blob = dir.path().join("blob.rlib");
    let output = dir.path().join("libprobe.rlib");
    std::fs::write(&blob, b"original").unwrap();
    write_authoritative_blob_digest(&blob).unwrap();
    std::fs::hard_link(&blob, &output).unwrap();
    set_readonly(&blob, true).unwrap();
    assert!(is_sealed(&blob).unwrap(), "legacy seals must be detected");

    // Same-inode hit must upgrade too, rather than returning before sealing.
    materialize_cached_file_with_mode(
        &output,
        &blob,
        crate::compiler::DeliveryPolicy::HardlinkEligible,
        MaterializationMode::Link,
        false,
    )
    .unwrap();
    assert!(crate::platform::fs::identity::same_file(&blob, &output).unwrap());
    assert_write_denied(&output, dir.path());
    let replacement = dir.path().join("replacement");
    std::fs::write(&replacement, b"rebuilt").unwrap();
    crate::platform::fs::replace::atomic_replace(&replacement, &output).unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), b"rebuilt");
    assert_eq!(std::fs::read(&blob).unwrap(), b"original");
    make_writable(&blob).unwrap();
}

#[test]
fn write_seal_switch_removes_current_and_legacy_seals() {
    let dir = tempfile::tempdir().unwrap();
    let blob = dir.path().join("blob.rmeta");
    std::fs::write(&blob, b"original").unwrap();
    for legacy in [true, false] {
        if legacy {
            set_readonly(&blob, true).unwrap();
        } else {
            seal_cache_blob(&blob).unwrap();
        }
        assert!(is_sealed(&blob).unwrap());
        set_cache_blob_sealed(&blob, false).unwrap();
        assert!(!is_sealed(&blob).unwrap());
        std::fs::write(&blob, b"writable").unwrap();
    }
}

#[test]
fn perf_write_seal_repeated_hit_sealing_budget() {
    let dir = tempfile::tempdir().unwrap();
    let blob = dir.path().join("blob.rmeta");
    std::fs::write(&blob, b"original").unwrap();
    seal_cache_blob(&blob).unwrap();
    // Bound repeated seal calls on the existing hardlink-hit path, including
    // the Windows facade's idempotent ACL lookup. No compiler or spawn timing.
    let start = Instant::now();
    for _ in 0..100 {
        seal_cache_blob(&blob).unwrap();
    }
    let elapsed_ns = start.elapsed().as_nanos();
    assert!(
        elapsed_ns < Duration::from_secs(1).as_nanos(),
        "100 seals took {elapsed_ns}ns"
    );
    assert_write_denied(&blob, dir.path());
    make_writable(&blob).unwrap();
}
