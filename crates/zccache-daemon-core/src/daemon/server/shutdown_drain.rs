//! Persistence barriers shared by standalone shutdown and restart tests.

use super::shutdown_completion::ShutdownError;
use super::*;

const INDEX_WRITER_SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const INDEX_WRITER_SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_secs(5);
const PENDING_WRITES_SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Quiesce publishers before flushing the #1954 journal. The returned permit
/// must stay alive until the caller finishes its depgraph/fingerprint saves.
pub(super) async fn drain_durable_state_for_shutdown(
    state: &SharedState,
    writer: Option<kernal_api::async_engine::Task<()>>,
) -> Result<crate::core::sync::PublicationDrainPermit, ShutdownError> {
    if !drain_pending_writes(state, PENDING_WRITES_SHUTDOWN_DRAIN_TIMEOUT).await {
        return Err(ShutdownError::BudgetExceeded);
    }
    let publication = state
        .artifact_publication
        .try_drain_for(crate::core::sync::OPERATION_TIMEOUT)
        .await
        .map_err(|_| ShutdownError::BudgetExceeded)?;
    let flush_result =
        flush_index_writer_result(&state.index_writer_tx, INDEX_WRITER_SHUTDOWN_DRAIN_TIMEOUT)
            .await;
    if flush_result.is_err() {
        record_shutdown_shortfall(
            state,
            "index_writer_drain: journal flush failed or was not acknowledged",
        );
    }
    state.index_writer_shutdown.notify_one();
    if let Some(mut writer) = writer {
        match kernal_api::async_engine::timeout(INDEX_WRITER_SHUTDOWN_JOIN_TIMEOUT, &mut writer)
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                record_shutdown_shortfall(state, &format!("index writer failed: {error}"))
            }
            Err(_) => {
                record_shutdown_shortfall(state, "index writer join budget exceeded");
                writer.cancel();
                return Err(ShutdownError::BudgetExceeded);
            }
        }
    }
    // Include direct store inserts and retry any journal rows retained after a
    // failed periodic publication. Do not consume the dirty bit before success.
    if state.artifact_store.is_dirty() || state.artifact_store.has_pending_entries() {
        let root = state
            .cache_root_lock
            .lease_for(STATE_TIMEOUT)
            .ok_or(ShutdownError::Persistence)?;
        let store = Arc::clone(&state.artifact_store);
        let result = kernal_api::async_engine::launch_blocking(move || {
            let _root = root;
            store.flush()
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            record_shutdown_shortfall(state, "artifact journal final flush failed");
        }
    }
    flush_result?;
    Ok(publication)
}

pub(super) async fn drain_pending_writes(state: &SharedState, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    let drained = pending_writes::await_all(&state.pending_cache_writes, timeout).await;
    let elapsed_ns = start.elapsed().as_nanos() as u64;
    tracing::info!(
        drained,
        elapsed_ns,
        "pending artifact writes drained before journal flush"
    );
    if !drained {
        record_shutdown_shortfall(
            state,
            "pending_writes: artifact writes exceeded shutdown budget",
        );
    }
    drained
}

/// Remove a record left by an earlier daemon on this cache root.
pub(super) fn clear_shutdown_shortfall(state: &SharedState) {
    let path = crate::core::config::shutdown_shortfall_path_from_cache_dir(&state.cache_dir);
    let _ = std::fs::remove_file(path.as_path());
}

/// Record a failed drain in memory and on disk for lifecycle diagnostics.
pub(super) fn record_shutdown_shortfall(state: &SharedState, reason: &str) {
    state
        .shutdown_completion
        .failed
        .store(true, Ordering::Release);
    tracing::warn!(reason, "shutdown persistence incomplete");
    crate::core::lifecycle::write_event_in_cache_root(
        state.cache_dir.as_path(),
        crate::core::lifecycle::EVENT_EMBEDDED_FLUSH_STEP_TIMEOUT,
        serde_json::json!({"step": "shutdown_persistence", "reason": reason}),
    );
    use std::io::Write as _;
    let path = crate::core::config::shutdown_shortfall_path_from_cache_dir(&state.cache_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.as_path())
        .and_then(|mut file| writeln!(file, "{reason}"));
    if let Err(error) = written {
        tracing::warn!(%error, reason, "could not record shutdown shortfall");
    }
}
