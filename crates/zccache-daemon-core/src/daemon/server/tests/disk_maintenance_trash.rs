//! Retired names still consume the cache budget until unlink succeeds.
use super::*;

#[test]
fn deferred_trash_counts_against_usage_until_recovered() {
    let root = tempfile::tempdir().unwrap();
    let artifact_dir = root.path().join("artifacts");
    let trash = artifact_dir.join(".staged-v2/.trash/abandoned");
    std::fs::create_dir_all(&trash).unwrap();
    let payload = trash.join("payload");
    std::fs::write(&payload, vec![7u8; 16 * 1024]).unwrap();
    let allocated = crate::platform::fs::volume::allocated_bytes(
        &payload,
        &std::fs::metadata(&payload).unwrap(),
    );
    assert!(allocated > 0);
    let store = ArtifactStore::open_empty(&root.path().join("index.bin"));
    let report = maintain_disk_artifacts(MaintenancePass {
        artifact_dir: &artifact_dir,
        artifacts: &DashMap::new(),
        artifact_store: &store,
        index_writer_tx: None,
        dep_graph: &DepGraph::new(),
        pending_write_bytes: 17,
        policy: bytes_policy(GIB),
        kind: MaintenanceKind::Full,
        environment: &FixedEnvironment {
            now: SystemTime::now(),
            space: FilesystemSpace {
                capacity_bytes: 1000 * GIB,
                free_bytes: 500 * GIB,
            },
        },
        retired_top_level: None,
    })
    .unwrap();
    assert_eq!(
        report.usage_before_bytes,
        allocated + 17,
        "durable trash disappeared from accounted usage before unlink"
    );
    assert_eq!(report.usage_after_bytes, 17);
    assert!(!payload.exists());
    assert_eq!(report.artifacts_removed, 0);
}

#[test]
fn a_failed_unlink_remains_accounted_while_successful_siblings_are_credited() {
    use crate::daemon::server::persist::{StagedFaultGuard, StagedFaultPoint};
    let root = tempfile::tempdir().unwrap();
    let artifact_dir = root.path().join("artifacts");
    let trash = artifact_dir.join(".staged-v2/.trash/abandoned");
    std::fs::create_dir_all(&trash).unwrap();
    let failed = trash.join("failed");
    let removed = trash.join("removed");
    std::fs::write(&failed, vec![1u8; 8192]).unwrap();
    std::fs::write(&removed, vec![2u8; 16384]).unwrap();
    let allocation = |path: &Path| {
        crate::platform::fs::volume::allocated_bytes(path, &std::fs::metadata(path).unwrap())
    };
    let failed_bytes = allocation(&failed);
    let removed_bytes = allocation(&removed);
    let expected_credit = if crate::core::config::file_may_free_space_on_removal(&removed) {
        removed_bytes
    } else {
        0
    };
    let fault = StagedFaultGuard::arm(&failed, [StagedFaultPoint::TrashDelete]);
    let report = pass(&artifact_dir, root.path());
    fault.assert_all_consumed();
    assert_eq!(report.usage_before_bytes, failed_bytes + removed_bytes + 17);
    assert_eq!(report.usage_after_bytes, failed_bytes + 17);
    assert_eq!(report.bytes_reclaimed, expected_credit);
    assert!(failed.exists());
    assert!(!removed.exists());
    drop(fault);
    let retry = pass(&artifact_dir, root.path());
    assert_eq!(retry.usage_before_bytes, failed_bytes + 17);
    assert_eq!(retry.usage_after_bytes, 17);
    assert!(!failed.exists());
}

#[test]
fn live_and_retired_hardlinks_are_counted_once_without_promising_free_space() {
    let root = tempfile::tempdir().unwrap();
    let artifact_dir = root.path().join("artifacts");
    let trash = artifact_dir.join(".staged-v2/.trash/abandoned");
    std::fs::create_dir_all(&trash).unwrap();
    let live = artifact_dir.join("retained_0");
    let retired = trash.join("same-inode");
    std::fs::write(&live, vec![3u8; 8192]).unwrap();
    std::fs::hard_link(&live, &retired).unwrap();
    let allocated =
        crate::platform::fs::volume::allocated_bytes(&live, &std::fs::metadata(&live).unwrap());
    let report = pass(&artifact_dir, root.path());
    assert_eq!(report.usage_before_bytes, allocated + 17);
    assert_eq!(report.usage_after_bytes, allocated + 17);
    assert_eq!(report.bytes_reclaimed, 0);
    assert!(live.exists());
    assert!(!retired.exists());
}

fn pass(artifact_dir: &Path, root: &Path) -> DiskMaintenanceReport {
    let store = ArtifactStore::open_empty(&root.join("index.bin"));
    maintain_disk_artifacts(MaintenancePass {
        artifact_dir,
        artifacts: &DashMap::new(),
        artifact_store: &store,
        index_writer_tx: None,
        dep_graph: &DepGraph::new(),
        pending_write_bytes: 17,
        policy: bytes_policy(GIB),
        kind: MaintenanceKind::Full,
        environment: &FixedEnvironment {
            now: SystemTime::now(),
            space: FilesystemSpace {
                capacity_bytes: 1000 * GIB,
                free_bytes: 500 * GIB,
            },
        },
        retired_top_level: None,
    })
    .unwrap()
}
