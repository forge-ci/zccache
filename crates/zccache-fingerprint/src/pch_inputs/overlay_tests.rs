//! Filesystem regressions independent of the installed clang version.

use super::*;
use kernal_api::platform::fs::{set_file_mtime, FileTime};
use std::fs;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, PchServePlan) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("header.h");
    let bytes = b"#pragma once\nint value;\n";
    fs::write(&path, bytes).unwrap();
    set_file_mtime(&path, FileTime::from_unix_time(1_700_000_000, 123_456_789)).unwrap();
    let plan = PchServePlan {
        inputs_checked: 1,
        replays: vec![PendingReplay {
            path: NormalizedPath::new(&path),
            recorded_secs: 1_600_000_000,
            current_secs: 1_700_000_000,
            size: bytes.len() as u64,
            content_xxh3: Some(content_xxh3(bytes)),
        }],
    };
    (dir, path, plan)
}

fn mtime(path: &Path) -> FileTime {
    FileTime::from_last_modification_time(&fs::metadata(path).unwrap())
}

#[test]
fn overlay_preserves_original_names_and_uses_independent_snapshots() {
    let (_dir, path, plan) = fixture();
    let private = tempfile::tempdir().unwrap();
    let original_time = mtime(&path);
    let overlay = write_pch_overlay(&plan, private.path()).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&fs::read(overlay).unwrap()).unwrap();
    assert_eq!(json["use-external-names"], false);
    assert_eq!(json["roots"][0]["name"], path.to_string_lossy().as_ref());
    let snapshot = Path::new(json["roots"][0]["external-contents"].as_str().unwrap());
    assert_eq!(fs::read(snapshot).unwrap(), fs::read(&path).unwrap());
    assert_eq!(mtime(snapshot), FileTime::from_unix_time(1_600_000_000, 0));
    assert_eq!(mtime(&path), original_time);
    fs::write(snapshot, b"private snapshot modified").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"#pragma once\nint value;\n");
    assert_eq!(mtime(&path), original_time);
}

#[test]
fn changed_content_before_snapshot_refuses_without_touching_source() {
    let (_dir, path, plan) = fixture();
    let private = tempfile::tempdir().unwrap();
    // Preserve size so only the verified content hash detects the change.
    fs::write(&path, b"#pragma once\nint other;\n").unwrap();
    let changed_time = mtime(&path);
    let refusal = write_pch_overlay(&plan, private.path()).unwrap_err();
    assert_eq!(refusal.reason, PchRefusalReason::ChangedDuringReplay);
    assert_eq!(mtime(&path), changed_time);
    assert!(!private.path().join("overlay.json").exists());
}

#[test]
fn imported_pch_size_change_before_snapshot_refuses() {
    let (_dir, path, mut plan) = fixture();
    plan.replays[0].content_xxh3 = None;
    let private = tempfile::tempdir().unwrap();
    fs::write(&path, b"a different size").unwrap();
    let changed_time = mtime(&path);
    let refusal = write_pch_overlay(&plan, private.path()).unwrap_err();
    assert_eq!(refusal.reason, PchRefusalReason::ChangedDuringReplay);
    assert_eq!(mtime(&path), changed_time);
    assert!(!private.path().join("overlay.json").exists());
}

#[test]
fn snapshot_without_verified_content_identity_is_refused() {
    let (_dir, path, mut plan) = fixture();
    plan.replays[0].content_xxh3 = None;
    let private = tempfile::tempdir().unwrap();
    let original_time = mtime(&path);
    assert_eq!(
        write_pch_overlay(&plan, private.path()).unwrap_err().reason,
        PchRefusalReason::MtimeUnverifiable
    );
    assert_eq!(mtime(&path), original_time);
}

#[test]
fn private_file_collision_refuses_without_overwriting_anything() {
    let (_dir, path, plan) = fixture();
    let private = tempfile::tempdir().unwrap();
    fs::write(private.path().join("input-0"), b"already owned").unwrap();
    let source_time = mtime(&path);
    assert!(matches!(
        write_pch_overlay(&plan, private.path()).unwrap_err().reason,
        PchRefusalReason::ReplayFailed(_)
    ));
    assert_eq!(
        fs::read(private.path().join("input-0")).unwrap(),
        b"already owned"
    );
    assert_eq!(mtime(&path), source_time);
    assert!(!private.path().join("overlay.json").exists());
}

#[test]
fn conflicting_timestamp_plans_refuse_without_source_mutation() {
    let (_dir, path, mut plan) = fixture();
    let private = tempfile::tempdir().unwrap();
    let mut conflicting = plan.replays[0].clone();
    conflicting.recorded_secs += 1;
    plan.replays.push(conflicting);
    let source_time = mtime(&path);
    assert_eq!(
        write_pch_overlay(&plan, private.path()).unwrap_err().reason,
        PchRefusalReason::ChangedDuringReplay
    );
    assert_eq!(mtime(&path), source_time);
    assert!(!private.path().join("overlay.json").exists());
}

#[test]
fn consistent_duplicate_plans_share_one_snapshot() {
    let (_dir, _path, mut plan) = fixture();
    let private = tempfile::tempdir().unwrap();
    plan.replays.push(plan.replays[0].clone());
    let overlay = write_pch_overlay(&plan, private.path()).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&fs::read(overlay).unwrap()).unwrap();
    assert_eq!(json["roots"].as_array().unwrap().len(), 1);
    assert!(!private.path().join("input-1").exists());
}

#[test]
fn forced_snapshot_freezes_matching_timestamp_and_rechecks_content() {
    let (_dir, path, original) = fixture();
    let pending = &original.replays[0];
    let mut input = PchInput {
        path: path.to_string_lossy().into_owned(),
        as_requested: String::new(),
        size: pending.size,
        mtime_secs: pending.current_secs as u64,
        content_xxh3: pending.content_xxh3,
        overridden: false,
        transient: false,
        top_level: false,
        imported_pch: true,
    };
    let mut plan = PchServePlan::default();
    plan.require_snapshot(&input).unwrap();
    plan.require_snapshot(&input).unwrap();
    assert_eq!(plan.replays.len(), 1);
    input.mtime_secs += 1;
    assert_eq!(
        plan.require_snapshot(&input).unwrap_err().reason,
        PchRefusalReason::ChangedDuringReplay
    );
    let private = tempfile::tempdir().unwrap();
    write_pch_overlay(&plan, private.path()).unwrap();
    fs::write(&path, b"#pragma once\nint other;\n").unwrap();
    let private = tempfile::tempdir().unwrap();
    assert_eq!(
        write_pch_overlay(&plan, private.path()).unwrap_err().reason,
        PchRefusalReason::ChangedDuringReplay
    );
}
