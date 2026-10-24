//! Artifact hydration started by the daemon run loop.
use super::*;

/// Start index hydration with publication ownership acquired before spawn.
/// The optional gate is a deterministic test seam for Clear/startup races.
pub(in crate::daemon::server) async fn spawn_artifact_loader(
    state: Arc<SharedState>,
    start_gate: Option<Arc<Notify>>,
) -> kernal_api::async_engine::Task<()> {
    // Acquire before spawning so Clear cannot overtake the startup loader and
    // then have pre-Clear entries inserted afterward.
    let publication_guard = Arc::clone(&state.artifact_publication)
        .read_for(crate::core::sync::OPERATION_TIMEOUT)
        .await;
    kernal_api::async_engine::launch(async move {
        if let Some(gate) = start_gate {
            gate.notified().await;
        }
        let artifact_dir = state.artifact_dir.clone();
        let state_ref = Arc::clone(&state);
        let loaded = kernal_api::async_engine::launch_blocking(move || {
            let _publication_guard = publication_guard;
            // Load the in-memory index that `ArtifactStore::open` already
            // hydrated from the on-disk blob.
            let entries = state_ref.artifact_store.load_all();
            if !entries.is_empty() {
                let count = entries.len();
                for (key, meta) in entries {
                    state_ref
                        .artifacts
                        .entry(key)
                        .or_insert_with(|| CachedArtifact::from_index(meta));
                }
                count
            } else {
                // Migration: legacy `.meta` files predate the current bincode
                // index blob; populate the live store from them
                // so the first session after upgrade still has its warm cache.
                migrate_meta_files(
                    &artifact_dir,
                    &state_ref.artifacts,
                    &state_ref.artifact_store,
                )
            }
        })
        .await
        .unwrap_or(0);
        if loaded > 0 {
            tracing::info!(loaded, "background artifact loading complete");
        }
        state.artifacts_loaded.store(true, Ordering::Release);
    })
}
