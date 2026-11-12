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
    let cached = ProvisionalArtifact::new(&artifact, Vec::new());
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
        publish_artifact_paths_observed(state, &key_hex, cached.meta().clone(), &sources)?;
        return Ok(Some(cached.durable()));
    }
    {
        let key_for_persist = key_hex.clone();
        let payloads: Vec<Arc<Vec<u8>>> = artifact
            .outputs
            .iter()
            .filter_map(|o| o.payload.as_bytes().cloned())
            .collect();
        let persist_meta = cached.meta().clone();
        if payloads.is_empty() {
            // Stream-only results live entirely in the final index flush.
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
        retired = cached.publish(&state.artifacts, key_hex.clone());
        let persist_state = Arc::clone(state);
        let failure_state = Arc::clone(state);
        let sem = Arc::clone(&state.persist_semaphore);
        let publication_guard_for_task =
            publication_guard.expect("direct exec publication acquired an owned guard");
        kernal_api::async_engine::launch(async move {
            // Release publication before the root lease on cancellation.
            let _ = &publication_guard_for_task;
            let _ = &persistence_root;
            match sem.try_acquire_for(admission_bound).await {
                Ok(_permit) => {
                    let written = kernal_api::async_engine::launch_blocking(move || {
                        let _ = &publication_guard_for_task;
                        let _cache_lifetime = persistence_root;
                        let _publication_guard = publication_guard_for_task;
                        let _unwind = persist_state.shutdown_completion.watch_unwind();
                        if let Err(error) = cached.persist_bytes(&persist_state, &key_for_persist, &payloads) {
                            tracing::warn!(%error, "exec artifact persistence failed; retaining resident payloads");
                        }
                    })
                    .detach_on_drop()
                    .await;
                    if let Err(error) = written {
                        failure_state.shutdown_completion.fail();
                        tracing::warn!(%error, "exec artifact persistence task failed to join");
                    }
                }
                Err(error) => {
                    failure_state.shutdown_completion.fail();
                    tracing::warn!(%error, ?admission_bound, "exec persistence admission expired");
                }
            }
            if let Some(gate) = completion_gate {
                gate.persisted.notify_one();
            }
        })
        .detach();
        // Persistence owns publication before retirement is scheduled.
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
