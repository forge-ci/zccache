//! Exec persistence and owned publication/retirement handoff.
use super::*;

pub(super) async fn store_exec_artifact_inner(
    state: &Arc<SharedState>,
    key_hex: String,
    artifact: ArtifactData,
    staged_sources: Option<Vec<NormalizedPath>>,
    handoff_gate: Option<Arc<PublicationHandoffGate>>,
    admission_bound: std::time::Duration,
) -> Result<Option<CachedArtifact>, StagedPublishFailure> {
    let cached = CachedArtifact::from_artifact_data(&artifact);
    let retired;
    let publication_guard = if staged_sources.is_none() {
        let Some(guard) = begin_artifact_publication(state).await else {
            return Ok(None);
        };
        Some(guard)
    } else {
        None
    };
    if let Some(sources) = staged_sources {
        publish_artifact_paths_observed(state, &key_hex, cached.meta.clone(), &sources)?;
        return Ok(Some(cached));
    }
    {
        let artifact_dir = state.artifact_dir.clone();
        let key_for_persist = key_hex.clone();
        let payloads: Vec<Arc<Vec<u8>>> = artifact
            .outputs
            .iter()
            .filter_map(|o| o.payload.as_bytes().cloned())
            .collect();
        let persist_meta = cached.meta.clone();
        if payloads.is_empty() {
            // Stdout/stderr-only artifacts have no payload files to persist,
            // so expose their index entry to the final shutdown flush now.
            state.artifact_store.insert(&key_hex, &persist_meta);
        }
        let completion_gate = if let Some(gate) = handoff_gate {
            gate.acquired.notify_one();
            gate.resume.notified().await;
            Some(gate)
        } else {
            None
        };
        let persistence_root = state
            .cache_root_lock
            .lease_for(crate::core::sync::STATE_TIMEOUT)
            .ok_or(StagedPublishFailure::StoreSetup)?;
        let retirement_root = state
            .cache_root_lock
            .lease_for(crate::core::sync::STATE_TIMEOUT)
            .ok_or(StagedPublishFailure::StoreSetup)?;
        retired = state.artifacts.insert(key_hex.clone(), cached);
        let sem = Arc::clone(&state.persist_semaphore);
        let publication_guard_for_task =
            publication_guard.expect("direct exec publication acquired an owned guard");
        let index_writer_tx = state.index_writer_tx.clone();
        kernal_api::async_engine::launch(async move {
            // Release publication before the root lease on cancellation.
            let _ = &publication_guard_for_task;
            let _ = &persistence_root;
            match kernal_api::async_engine::timeout(admission_bound, sem.acquire()).await {
                Ok(Ok(_permit)) => {
                    let written = kernal_api::async_engine::launch_blocking(move || {
                        let _ = &publication_guard_for_task;
                        let _cache_lifetime = persistence_root;
                        let _publication_guard = publication_guard_for_task;
                        if persist_artifact_payloads(&artifact_dir, &key_for_persist, &payloads)
                            .is_ok()
                        {
                            let _ = index_writer_tx
                                .send(IndexWriterCommand::Insert(key_for_persist, persist_meta));
                        }
                    })
                    .detach_on_drop()
                    .await;
                    if let Err(error) = written {
                        tracing::warn!(%error, "exec artifact persistence task failed to join");
                    }
                }
                Ok(Err(error)) => tracing::warn!(%error, "exec persistence admission closed"),
                Err(_) => tracing::warn!(?admission_bound, "exec persistence admission expired"),
            }
            if let Some(gate) = completion_gate {
                gate.persisted.notify_one();
            }
        })
        .detach();
        // No further suspension: publication has moved to the persistence
        // owner before old records are scheduled on their blocking worker.
        if retired.is_some() {
            let cleanup = super::super::cached_artifact_retirement::retire_owned_for(
                retirement_root,
                retired.into_iter().collect(),
                crate::core::sync::OPERATION_TIMEOUT,
            );
            kernal_api::async_engine::launch(cleanup).detach();
        }
    }
    Ok(None)
}
