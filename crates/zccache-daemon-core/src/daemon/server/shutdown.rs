//! Ordered, bounded standalone persistence shutdown, reusing the journal writer.

use super::shutdown_completion::ShutdownError;
use super::*;
type Worker = Option<kernal_api::async_engine::Task<()>>;

pub(super) async fn run(
    state: &Arc<SharedState>,
    maintenance: Worker,
    depgraph: Worker,
    writer: Worker,
) {
    let result = match kernal_api::async_engine::timeout(
        state.shutdown_completion.budget,
        drain(state, maintenance, depgraph, writer),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(ShutdownError::BudgetExceeded),
    };
    if let Err(error) = result {
        record_shutdown_shortfall(state, &format!("{error:?}"));
    }
    state.shutdown_completion.finish(result).await;
}

async fn drain(
    state: &Arc<SharedState>,
    maintenance: Worker,
    depgraph: Worker,
    writer: Worker,
) -> Result<(), ShutdownError> {
    if let Some(handle) = maintenance {
        handle.await.map_err(|_| ShutdownError::Persistence)?;
    }
    if let Some(handle) = depgraph {
        handle.await.map_err(|_| ShutdownError::Persistence)?;
    }
    state
        .watcher
        .try_with_for(Duration::from_secs(5), |watcher| *watcher = None)
        .await
        .map_err(|_| ShutdownError::BudgetExceeded)?;
    state.watcher_active.store(false, Ordering::Release);
    // Every detached compile/link/exec owns a publication read permit through
    // its payload/index send. Compile continuations also have pending-write
    // registrations. Directory links publish synchronously under that permit.
    let _publication = drain_durable_state_for_shutdown(state, writer).await?;
    if state
        .artifacts
        .iter()
        .any(|entry| entry.value().is_provisional())
    {
        record_shutdown_shortfall(
            state,
            "artifact payload publication failed; resident entries remain",
        );
    }
    save_depgraph(state).await;
    save_metadata(state).await;
    save_compiler_hashes(state).await;
    save_system_includes(state).await;
    let _ = std::fs::remove_dir_all(&state.depfile_tmpdir);
    if state.shutdown_completion.failed.load(Ordering::Acquire) {
        return Err(ShutdownError::Persistence);
    }
    // Release only after all publishers and snapshots have finished. On error
    // outstanding owners retain the lease until they stop writing.
    state.cache_root_lock.release();
    Ok(())
}

async fn save_depgraph(state: &Arc<SharedState>) {
    let graph = state.dep_graph.load_full();
    if !graph.take_dirty() && !graph.has_pending_contexts() {
        return;
    }
    let path = depgraph_file_path_for_cache_dir(&state.cache_dir);
    let result = run_depgraph_save_with(Arc::clone(state), None, move |graph| {
        crate::depgraph::save_to_file(graph, &path)
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|saved| saved.map_err(|error| error.to_string()));
    if result.is_ok() {
        state.dep_graph_persisted.store(true, Ordering::Release);
    }
    record_save_result(state, "depgraph", result);
}

async fn save_metadata(state: &Arc<SharedState>) {
    if !state.metadata_cache_loaded.load(Ordering::Acquire)
        || !state.cache_system.metadata().take_dirty()
    {
        return;
    }
    let owner = Arc::clone(state);
    let result = kernal_api::async_engine::launch_blocking(move || {
        owner
            .cache_system
            .metadata()
            .save_to_disk(owner.metadata_path.as_path())
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|saved| saved.map_err(|error| error.to_string()));
    record_save_result(state, "metadata", result);
}

async fn save_compiler_hashes(state: &Arc<SharedState>) {
    if !state.compiler_hash_cache_loaded.load(Ordering::Acquire)
        || !state.compiler_hash_cache.take_dirty()
    {
        return;
    }
    let owner = Arc::clone(state);
    let result = kernal_api::async_engine::launch_blocking(move || {
        owner
            .compiler_hash_cache
            .save_to_disk(owner.compiler_hash_cache_path.as_path())
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|saved| saved.map_err(|error| error.to_string()));
    record_save_result(state, "compiler hashes", result);
}

async fn save_system_includes(state: &Arc<SharedState>) {
    if !state.system_includes_loaded.load(Ordering::Acquire) {
        return;
    }
    let includes = state
        .system_includes
        .with_for(STATE_TIMEOUT, |includes| {
            includes.take_dirty().then(|| includes.clone())
        })
        .await;
    if let Some(includes) = includes {
        let path = state.system_includes_cache_path.clone();
        let result = kernal_api::async_engine::launch_blocking(move || {
            includes.save_to_disk(path.as_path())
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|saved| saved.map_err(|error| error.to_string()));
        record_save_result(state, "system includes", result);
    }
}

fn record_save_result(state: &SharedState, name: &str, result: Result<(), String>) {
    if let Err(error) = result {
        tracing::warn!(name, %error, "shutdown snapshot failed");
        record_shutdown_shortfall(state, &format!("{name} save failed: {error}"));
    }
}
