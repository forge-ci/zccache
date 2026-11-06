//! Validate cached clang PCH inputs without changing shared source files.
//!
//! Content-identical timestamp drift may still reuse the cached PCH. When
//! zccache later runs a consumer, `pch_overlay` supplies immutable snapshots
//! with the recorded metadata through a private Clang VFS overlay. Rewinding
//! a real header would hide edits from other build directories.

use super::super::*;
use super::cached_hit::CachedHitFailure;
use crate::fingerprint::pch_inputs::{
    parse_pch_inputs, plan_pch_serve, read_pch_inputs, PchInputsError, PchServeOptions,
};

/// Whether `output` names a precompiled header (clang writes `CPCH` to both
/// `.pch` and `.gch`; a GCC `.gch` is recognised by content and passed).
pub(in crate::daemon::server) fn is_pch_output(output: &Path) -> bool {
    matches!(
        output.extension().and_then(|ext| ext.to_str()),
        Some("pch" | "gch")
    )
}

pub(super) struct PchGateRequest<'a> {
    pub(super) state: &'a SharedState,
    pub(super) sid: &'a SessionId,
    pub(super) payload: &'a CachedPayload,
    pub(super) provenance: Option<&'a CachedPayload>,
    pub(super) source_path: &'a NormalizedPath,
    pub(super) output_path: &'a NormalizedPath,
    pub(super) replay_root: Option<&'a NormalizedPath>,
    pub(super) replay_enabled: bool,
}

/// Validate a cached PCH payload before serving; a refusal becomes a miss.
/// A consumer launched through zccache obtains any required private view.
pub(super) fn gate_cached_pch(request: PchGateRequest<'_>) -> Result<(), CachedHitFailure> {
    let PchGateRequest {
        state,
        sid,
        payload,
        provenance,
        source_path,
        output_path,
        replay_root,
        replay_enabled,
    } = request;
    let log = |line: String| write_session_log(&state.sessions, sid, &line);
    let parsed = match payload {
        CachedPayload::Bytes(bytes) => parse_pch_inputs(bytes),
        CachedPayload::File(path) => read_pch_inputs(path),
    };
    let inputs = match parsed {
        Ok(Some(inputs)) => inputs,
        // Not a clang PCH (e.g. a GCC `.gch`): nothing recorded to check.
        Ok(None) => return Ok(()),
        Err(error) => {
            log(format!(
                "[PCH_REFUSED] {} unreadable_records: {error}",
                output_path.display()
            ));
            return Err(match error {
                PchInputsError::Io(_) => CachedHitFailure::CacheRead,
                _ => CachedHitFailure::PchInputsStale,
            });
        }
    };
    if inputs.iter().any(|input| input.imported_pch) {
        super::pch_provenance::verify_cached_imports(payload, provenance).map_err(|error| {
            log(format!(
                "[PCH_REFUSED] {} import provenance: {error}",
                output_path.display()
            ));
            CachedHitFailure::PchInputsStale
        })?;
    }
    let root = replay_root
        .map(NormalizedPath::as_path)
        .or_else(|| source_path.parent())
        .unwrap_or_else(|| source_path.as_path());
    let plan = plan_pch_serve(
        &inputs,
        PchServeOptions {
            replay_root: root,
            expected_top_level: Some(source_path.as_path()),
            replay_enabled,
        },
    )
    .map_err(|refusal| {
        log(format!(
            "[PCH_REFUSED] {} input {refusal}",
            output_path.display()
        ));
        tracing::info!(pch = %output_path.display(), %refusal, "cached PCH refused");
        CachedHitFailure::PchInputsStale
    })?;
    for input in &plan.replays {
        log(format!(
            "[PCH_INPUT_VIEW_PENDING] {} recorded mtime {} (source unchanged) for {}",
            input.path.display(),
            input.recorded_secs,
            output_path.display(),
        ));
    }
    Ok(())
}

/// The PCH binaries a compile loads: every `-include-pch` / `.gch` force
/// include, plus the `<header>.pch` clang substitutes for `-include <header>`.
pub(super) fn loaded_pchs(force_includes: &[NormalizedPath]) -> Vec<NormalizedPath> {
    let mut pchs: Vec<NormalizedPath> = Vec::new();
    for path in force_includes {
        let candidate = if is_pch_output(path) {
            path.clone()
        } else {
            let mut implicit = path.as_os_str().to_owned();
            implicit.push(".pch");
            NormalizedPath::new(Path::new(&implicit))
        };
        if candidate.is_file() && !pchs.contains(&candidate) {
            pchs.push(candidate);
        }
    }
    pchs
}

/// `ZCCACHE_PCH_MTIME_REPLAY` from the requesting client's environment.
pub(super) fn replay_enabled_for(client_env: Option<&[(String, String)]>) -> bool {
    use crate::fingerprint::pch_inputs::{pch_mtime_replay_enabled, PCH_MTIME_REPLAY_ENV};
    pch_mtime_replay_enabled(client_env.and_then(|env| {
        env.iter()
            .find_map(|(key, value)| (key == PCH_MTIME_REPLAY_ENV).then_some(value.as_str()))
    }))
}

/// Key a PCH generation on its exact invocation (#1952): a PCH embeds its
/// inputs' spellings, so the key takes the cwd and every argument in order,
/// as spelled, not the normalized flag set ordinary objects share.
pub(super) fn salt_pch_key(
    ctx: &mut crate::depgraph::CompileContext,
    source_mode: crate::compiler::SourceMode,
    cwd: &Path,
    args: &[String],
) {
    if matches!(source_mode, crate::compiler::SourceMode::Header) {
        ctx.unknown_flags.push(exact_invocation_key_flag(cwd, args));
    }
}

/// Cache-key material: the working directory and every argument in order.
fn exact_invocation_key_flag(cwd: &Path, args: &[String]) -> String {
    let mut material = Vec::new();
    material.extend_from_slice(cwd.to_string_lossy().as_bytes());
    for arg in args {
        material.push(0);
        material.extend_from_slice(arg.as_bytes());
    }
    format!(
        "zccache-pch-invocation={}",
        crate::hash::hash_bytes(&material).to_hex()
    )
}
