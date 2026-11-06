//! Compiler-private metadata views for immutable cached PCH inputs.

use super::super::*;
use crate::fingerprint::pch_inputs::{
    content_xxh3, parse_pch_inputs, plan_pch_serve, read_pch_inputs, write_pch_overlay, PchInput,
    PchRefusal, PchRefusalReason, PchServeOptions,
};

/// Owns snapshot files until the compiler that sees them has exited.
pub(in crate::daemon::server) struct PchOverlay {
    _directory: tempfile::TempDir,
    path: NormalizedPath,
}

impl PchOverlay {
    pub(in crate::daemon::server) fn path(&self) -> &Path {
        &self.path
    }

    pub(in crate::daemon::server) fn append_args(&self, args: &mut Vec<String>) {
        let position = args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(args.len());
        args.splice(
            position..position,
            [
                "-ivfsoverlay".to_owned(),
                self.path.to_string_lossy().into_owned(),
            ],
        );
    }
}

/// Includes the transitive base PCHs: a chained PCH omits inherited headers.
fn collect_inputs(
    pch: &Path,
    visited: &mut std::collections::HashSet<NormalizedPath>,
    inputs: &mut Vec<PchInput>,
    freeze: bool,
    frozen: &mut Vec<PchInput>,
) -> Result<(), PchRefusal> {
    if !visited.insert(NormalizedPath::new(pch)) {
        return Ok(());
    }
    let fail = |error: String| PchRefusal {
        path: pch.to_string_lossy().into_owned(),
        reason: PchRefusalReason::Unreadable(error),
    };
    let records = if freeze {
        // Parse and freeze one byte identity before the producer executes.
        // Post-compile capture cannot distinguish a same-second replacement
        // from the unsigned imported AST actually consumed by Clang.
        let bytes = std::fs::read(pch).map_err(|error| fail(error.to_string()))?;
        let records = parse_pch_inputs(&bytes).map_err(|error| fail(error.to_string()))?;
        if records.is_some() {
            let metadata = std::fs::metadata(pch).map_err(|error| fail(error.to_string()))?;
            let seconds =
                kernal_api::platform::fs::FileTime::from_last_modification_time(&metadata)
                    .unix_seconds();
            let seconds = u64::try_from(seconds).map_err(|error| fail(error.to_string()))?;
            frozen.push(PchInput {
                path: pch.to_string_lossy().into_owned(),
                as_requested: pch.to_string_lossy().into_owned(),
                size: bytes.len() as u64,
                mtime_secs: seconds,
                content_xxh3: Some(content_xxh3(&bytes)),
                overridden: false,
                transient: false,
                top_level: false,
                imported_pch: true,
            });
        }
        records
    } else {
        read_pch_inputs(pch).map_err(|error| fail(error.to_string()))?
    };
    if let Some(mut records) = records {
        if records.iter().any(|record| record.imported_pch) {
            let hashes = if freeze {
                super::pch_provenance::verified_pch_import_hashes(pch)
            } else {
                super::pch_provenance::legacy_pch_import_hashes(pch)
            }
            .map_err(|error| PchRefusal {
                path: pch.to_string_lossy().into_owned(),
                reason: PchRefusalReason::Unreadable(error.to_string()),
            })?;
            // The writer rechecks this exact verified byte identity before
            // snapshotting, including a same-size replacement during setup.
            for record in records.iter_mut().filter(|record| record.imported_pch) {
                record.content_xxh3 = hashes.get(&record.path).copied();
            }
        }
        for record in &records {
            if record.imported_pch {
                collect_inputs(Path::new(&record.path), visited, inputs, freeze, frozen)?;
            }
        }
        inputs.extend(records);
    }
    Ok(())
}

/// Build a VFS overlay for drifted inputs without changing workspace files.
pub(in crate::daemon::server) fn prepare_loaded_pch_overlay(
    state: &SharedState,
    sid: &SessionId,
    force_includes: &[NormalizedPath],
    replay_root: &Path,
    client_env: Option<&[(String, String)]>,
    args: &[String],
    freeze_loaded_pchs: bool,
) -> Result<Option<PchOverlay>, PchRefusal> {
    let replay_enabled = super::pch_gate::replay_enabled_for(client_env);
    if force_includes.is_empty() || (!replay_enabled && !freeze_loaded_pchs) {
        return Ok(None);
    }
    let mut inputs = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut frozen = Vec::new();
    let loaded_pchs = super::pch_gate::loaded_pchs(force_includes);
    for pch in &loaded_pchs {
        collect_inputs(
            pch,
            &mut visited,
            &mut inputs,
            freeze_loaded_pchs,
            &mut frozen,
        )?;
    }
    if inputs.is_empty() {
        return Ok(None);
    }
    let mut records = std::collections::HashMap::new();
    for input in &inputs {
        if let Some(previous) = records.insert(&input.path, input) {
            if previous.size != input.size
                || previous.mtime_secs != input.mtime_secs
                || previous.content_xxh3 != input.content_xxh3
            {
                return Err(PchRefusal {
                    path: input.path.clone(),
                    reason: PchRefusalReason::Unreadable(
                        "loaded PCHs record conflicting metadata for this input".to_owned(),
                    ),
                });
            }
        }
    }
    let inputs: Vec<_> = records.into_values().cloned().collect();
    let mut plan = plan_pch_serve(
        &inputs,
        PchServeOptions {
            replay_root,
            expected_top_level: None,
            replay_enabled,
        },
    )?;
    for file in frozen {
        // Imported roots retain their parent's recorded timestamp, but must
        // have the same verified bytes we parsed before execution.
        let input = inputs.iter().find(|input| input.path == file.path);
        if let Some(input) = input {
            if input.size != file.size || input.content_xxh3 != file.content_xxh3 {
                return Err(PchRefusal {
                    path: file.path,
                    reason: PchRefusalReason::ChangedDuringReplay,
                });
            }
            plan.require_snapshot(input)?;
        } else {
            plan.require_snapshot(&file)?;
        }
    }
    if plan.replays.is_empty() {
        return Ok(None);
    }
    if !freeze_loaded_pchs {
        // Any private repair can hide the native failure of an unsigned
        // import, even when only a header needs a snapshot. Require every
        // imported parent baseline recursively before exposing such a view.
        for pch in &loaded_pchs {
            super::pch_provenance::verified_pch_import_hashes(pch).map_err(|error| PchRefusal {
                path: pch.to_string_lossy().into_owned(),
                reason: PchRefusalReason::Unreadable(error.to_string()),
            })?;
        }
    }
    // A caller overlay can change which bytes a recorded path names. Do not
    // replace that view using snapshots read from the physical filesystem.
    if args.iter().any(|arg| arg.starts_with("-ivfsoverlay")) {
        return Err(PchRefusal {
            path: replay_root.to_string_lossy().into_owned(),
            reason: PchRefusalReason::Unreadable(
                "cannot compose a PCH metadata view with a caller VFS overlay".to_owned(),
            ),
        });
    }
    let directory = tempfile::Builder::new()
        .prefix("pch-input-view-")
        .tempdir_in(&state.depfile_tmpdir)
        .map_err(|error| PchRefusal {
            path: replay_root.to_string_lossy().into_owned(),
            reason: PchRefusalReason::Unreadable(error.to_string()),
        })?;
    let path = write_pch_overlay(&plan, directory.path())?;
    for input in &plan.replays {
        write_session_log(
            &state.sessions,
            sid,
            &format!(
                "[PCH_INPUT_VIEW] {} snapshot mtime {} (source unchanged) for loaded PCH",
                input.path.display(),
                input.recorded_secs,
            ),
        );
    }
    Ok(Some(PchOverlay {
        _directory: directory,
        path,
    }))
}
