//! Clear retires live names under publication exclusion, then sweeps off-lane.
use super::*;

pub(super) async fn handle_clear(state: &Arc<SharedState>) -> Response {
    let maintenance = state.disk_maintenance.acquire_for(OPERATION_TIMEOUT).await;
    if !pending_writes::await_all(&state.pending_cache_writes, crate::core::wait::PROBE).await {
        return Response::Error {
            message: "pending artifact writes did not drain before Clear".into(),
        };
    }
    let Some(root) = state.cache_root_lock.lease_for(STATE_TIMEOUT) else {
        return Response::Error {
            message: "Clear skipped because cache-root ownership was released".into(),
        };
    };
    let worker_state = Arc::clone(state);
    match state
        .launch_blocking_with_root(root, move || {
            let state = worker_state;
            let _maintenance = maintenance;
            clear_on_blocking_lane(&state, crate::core::wait::PROBE)
        })
        .await
    {
        Ok(response) => response,
        Err(error) => Response::Error {
            message: format!("Clear worker failed: {error}"),
        },
    }
}

fn clear_on_blocking_lane(state: &SharedState, bound: std::time::Duration) -> Response {
    // Keep retired owners in the outer scope: even an error/panic releases
    // publication exclusion before a last private-plan owner can clean files.
    let mut retired_artifacts = Vec::new();
    let (
        artifacts_removed,
        metadata_cleared,
        dep_graph_contexts_cleared,
        staged,
        kv,
        index_cleared,
    ) = {
        let _publication = state
            .artifact_publication
            .blocking_write_for(OPERATION_TIMEOUT);
        let artifacts_removed = state.artifacts.len() as u64;
        let metadata_cleared = state.cache_system.metadata().len() as u64;
        let dep_graph_contexts_cleared = state.dep_graph.load().stats().context_count as u64;
        let keys: Vec<_> = state
            .artifacts
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        retired_artifacts.reserve(keys.len());
        for key in keys {
            if let Some((_, artifact)) = state.artifacts.remove(&key) {
                retired_artifacts.push(artifact);
            }
        }
        // Compiler environment caches self-verify and intentionally survive Clear (#558).
        state.dep_graph.load().clear();
        state.cache_system.clear();
        state.fast_hit_cache.clear();
        state.request_cache.clear();
        state.request_validation_cache.clear();
        state.rsp_cache.clear();
        state.watched_raw_dirs.clear();
        state
            .watched_dirs
            .with_blocking_for(STATE_TIMEOUT, |dirs| dirs.clear());
        let staged = match persist::detach_clear_artifact_paths_for(
            state.artifact_dir.as_path(),
            &state.cache_dir,
            bound,
        ) {
            Ok(detached) => detached,
            Err(error) => {
                return Response::Error {
                    message: format!("Clear artifact retirement failed: {error}"),
                }
            }
        };
        let kv = match state
            .exec_store
            .detach_namespace_for(handle_exec_probe::EXEC_PROBE_NAMESPACE, bound)
        {
            Ok(detached) => detached,
            Err(error) => {
                return Response::Error {
                    message: format!("Clear exec retirement failed: {error}"),
                }
            }
        };
        // Preserve actor ordering: old queued inserts must be cleared before
        // allowing a new publication to enqueue its own index row.
        let index_cleared = wal::clear_index_writer_blocking(&state.index_writer_tx, bound);
        state.stats.reset();
        state.profiler.reset();
        for profile in &state.session_staged_profiles {
            profile.value().reset();
        }
        (
            artifacts_removed,
            metadata_cleared,
            dep_graph_contexts_cleared,
            staged,
            kv,
            index_cleared,
        )
    };
    drop(retired_artifacts);
    #[cfg(test)]
    persist::StagedHookGuard::pause_kv_sweep(state.cache_dir.as_path());
    let mut on_disk_bytes_freed = 0u64;
    match staged.sweep_for(bound) {
        Ok(report) => {
            on_disk_bytes_freed = on_disk_bytes_freed.saturating_add(report.reclaimed_bytes)
        }
        Err(error) => tracing::warn!(%error, "detached Clear artifacts await recovery"),
    }
    match kv.sweep_for(bound) {
        Ok(report) => {
            on_disk_bytes_freed = on_disk_bytes_freed.saturating_add(report.reclaimed_bytes)
        }
        Err(error) => tracing::warn!(%error, "detached Clear exec namespace awaits recovery"),
    }
    // Retry durable names left by older failed or cancelled cleanup too.
    match persist::sweep_staged_trash_for(state.artifact_dir.as_path(), bound) {
        Ok(report) => {
            on_disk_bytes_freed = on_disk_bytes_freed.saturating_add(report.reclaimed_bytes);
            if report.failures > 0 || report.deferred {
                tracing::warn!(
                    failures = report.failures,
                    deferred = report.deferred,
                    "Clear left staged trash for the next maintenance pass"
                );
            }
        }
        Err(error) => tracing::warn!(%error, "Clear staged recovery deferred"),
    }
    on_disk_bytes_freed =
        on_disk_bytes_freed.saturating_add(recover_retired_exec_for(&state.exec_store, bound));
    if !index_cleared {
        return Response::Error {
            message: "persistent artifact index Clear failed or timed out".into(),
        };
    }
    tracing::info!(
        artifacts_removed,
        metadata_cleared,
        dep_graph_contexts_cleared,
        on_disk_bytes_freed,
        "cache cleared"
    );
    Response::Cleared {
        artifacts_removed,
        metadata_cleared,
        dep_graph_contexts_cleared,
        on_disk_bytes_freed,
    }
}
