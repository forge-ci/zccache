//! Compiler-private timestamp views; source files are never modified.

use super::{content_xxh3, PchRefusal, PchRefusalReason, PchServePlan};
use kernal_api::platform::fs::{set_file_mtime, FileTime};
use std::io::Write;
use std::path::Path;
use zccache_core::NormalizedPath;

/// Materialize a private VFS overlay for the drifted inputs in `plan`.
///
/// `directory` must be a caller-owned private directory, retained until the
/// compiler exits and never reused by another invocation. Each input is read
/// and verified again before creating an independent snapshot; only that
/// snapshot receives the recorded mtime. Every snapshot requires a verified
/// content hash. Callers must bind imported PCH hashes to producer provenance;
/// ordinary clang PCH imports have no signature. Producers freeze all loaded
/// PCH binaries before execution so provenance names the bytes clang used.
///
/// A failure can leave private snapshots for the caller to clean up, but
/// never writes to an original input, header or PCH. The overlay is published
/// only after all snapshots have been verified and written.
pub fn write_pch_overlay(
    plan: &PchServePlan,
    directory: &Path,
) -> Result<NormalizedPath, PchRefusal> {
    let directory =
        kernal_api::platform::fs::canonical_context_path(directory).map_err(|error| {
            PchRefusal {
                path: directory.to_string_lossy().into_owned(),
                reason: PchRefusalReason::ReplayFailed(error.to_string()),
            }
        })?;
    // LLVM's VFS uses ordinary Windows absolute names, including UNC names.
    let directory = NormalizedPath::new(zccache_core::path::strip_verbatim_prefix(&directory));
    let mut roots = Vec::with_capacity(plan.replays.len());
    for (index, replay) in plan.replays.iter().enumerate() {
        let refusal = |reason| PchRefusal {
            path: replay.path.to_string_lossy().into_owned(),
            reason,
        };
        if plan.replays[..index].iter().any(|previous| {
            previous.path == replay.path
                && (previous.recorded_secs != replay.recorded_secs
                    || previous.size != replay.size
                    || previous.content_xxh3 != replay.content_xxh3)
        }) {
            return Err(refusal(PchRefusalReason::ChangedDuringReplay));
        }
        if plan.replays[..index]
            .iter()
            .any(|previous| previous.path == replay.path)
        {
            continue;
        }
        let bytes = std::fs::read(&replay.path)
            .map_err(|error| refusal(PchRefusalReason::Unreadable(error.to_string())))?;
        if bytes.len() as u64 != replay.size {
            return Err(refusal(PchRefusalReason::ChangedDuringReplay));
        }
        let expected = replay
            .content_xxh3
            .ok_or_else(|| refusal(PchRefusalReason::MtimeUnverifiable))?;
        if content_xxh3(&bytes) != expected {
            return Err(refusal(PchRefusalReason::ChangedDuringReplay));
        }
        let snapshot = directory.join(format!("input-{index}"));
        let write_snapshot = || -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&snapshot)?;
            file.write_all(&bytes)?;
            let seconds = i64::try_from(replay.recorded_secs)
                .map_err(|_| std::io::Error::other("PCH mtime is out of range"))?;
            set_file_mtime(&snapshot, FileTime::from_unix_time(seconds, 0))
        };
        write_snapshot()
            .map_err(|error| refusal(PchRefusalReason::ReplayFailed(error.to_string())))?;
        roots.push(serde_json::json!({
            "type": "file",
            "name": replay.path.as_path(),
            "external-contents": snapshot,
        }));
    }
    let overlay = directory.join("overlay.json");
    let write_overlay = || -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&overlay)?;
        serde_json::to_writer(
            &mut file,
            &serde_json::json!({
                "version": 0,
                "use-external-names": false,
                "roots": roots,
            }),
        )
        .map_err(std::io::Error::other)
    };
    write_overlay().map_err(|error| PchRefusal {
        path: overlay.to_string_lossy().into_owned(),
        reason: PchRefusalReason::ReplayFailed(error.to_string()),
    })?;
    Ok(overlay)
}
