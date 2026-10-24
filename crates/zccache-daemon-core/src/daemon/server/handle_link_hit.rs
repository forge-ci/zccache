//! Link-cache hit materialization.
use super::*;

pub(super) enum LinkCacheHitOutcome {
    Hit(Box<Response>),
    Fallback,
    Miss,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn materialize_link_cache_hit(
    state: &SharedState,
    key_hex: &str,
    cwd_path: &Path,
    declared_output: &NormalizedPath,
    secondary_outputs: &[NormalizedPath],
    is_directory: bool,
    warning: Option<String>,
    mode: MaterializationMode,
) -> LinkCacheHitOutcome {
    let Some(entry) = lookup_artifact_with_disk_fallback(state, key_hex) else {
        return LinkCacheHitOutcome::Miss;
    };
    // Legacy link rows lost permissions when outputs became byte payloads.
    // Re-link instead of guessing which primary or side outputs are executable.
    if !is_directory && entry.meta.output_modes.len() != entry.meta.output_names.len() {
        return LinkCacheHitOutcome::Miss;
    }
    let Ok(payloads) = ensure_payloads_for_materialization_for_state(state, &entry, key_hex)
        .map_err(|failure| {
            report_materialization_failure(&state.cache_dir, key_hex, "link-hit", &failure);
        })
    else {
        return LinkCacheHitOutcome::Miss;
    };
    record_artifact_access(state, key_hex, &entry, std::time::Instant::now());
    let names = Arc::clone(&entry.meta.output_names);
    let exit_code = entry.meta.exit_code;
    let stdout = entry.stdout.clone();
    let stderr = entry.stderr.clone();
    tracing::debug!(%key_hex, "link cache hit");
    state.stats.record_link_hit();

    let output_path = if declared_output.is_absolute() {
        declared_output.clone()
    } else {
        cwd_path.join(declared_output).into()
    };
    if is_directory {
        let valid_bundle =
            payloads.len() == 1 && names.len() == 1 && is_directory_output_name(&names[0]);
        let materialize_started = std::time::Instant::now();
        payloads.record_staged_pre_materialization(&state.profiler.staged);
        let materialized =
            valid_bundle.then(|| materialize_directory_payload(&payloads[0], &output_path));
        payloads.record_staged_lock_timings(&state.profiler.staged);
        drop(payloads);
        // Remove a failed unpack's temp tree only after the lease is gone.
        let observed = materialized
            .and_then(|result| result.map_err(DirectoryUnpackFailure::cleanup).ok())
            .map(|copy_bytes| StagedMaterializationStats {
                copy_count: 1,
                copy_bytes,
                ..StagedMaterializationStats::default()
            });
        return if record_staged_hit_materialization(state, 1, materialize_started, observed) {
            LinkCacheHitOutcome::Hit(Box::new(Response::LinkResult {
                exit_code,
                stdout,
                stderr,
                cached: true,
                warning,
            }))
        } else {
            LinkCacheHitOutcome::Fallback
        };
    }

    let targets: Vec<NormalizedPath> = (0..payloads.len())
        .map(|i| {
            if i == 0 {
                output_path.clone()
            } else if let Some(secondary) = secondary_outputs.get(i - 1) {
                if secondary.is_absolute() {
                    secondary.clone()
                } else {
                    cwd_path.join(secondary).into()
                }
            } else {
                output_path
                    .parent()
                    .unwrap_or(cwd_path)
                    .join(&names[i])
                    .into()
            }
        })
        .collect();
    let has_staged_payload = payloads.iter().any(|payload| {
        matches!(payload, CachedPayload::File(path) if is_staged_artifact_path(path.as_path()))
    });
    let materialize_started = std::time::Instant::now();
    payloads.record_staged_pre_materialization(&state.profiler.staged);
    let observed =
        write_payloads_par_with_modes(&targets, &payloads, mode, &entry.meta.output_modes);
    payloads.record_staged_lock_timings(&state.profiler.staged);
    drop(payloads);
    if let Err(failure) = &observed {
        report_materialization_failure(&state.cache_dir, key_hex, "link-hit", failure);
    }
    let write_ok = if has_staged_payload {
        record_staged_hit_materialization(state, targets.len(), materialize_started, observed.ok())
    } else {
        observed.is_ok()
    };
    if write_ok {
        LinkCacheHitOutcome::Hit(Box::new(Response::LinkResult {
            exit_code,
            stdout,
            stderr,
            cached: true,
            warning,
        }))
    } else {
        LinkCacheHitOutcome::Fallback
    }
}
