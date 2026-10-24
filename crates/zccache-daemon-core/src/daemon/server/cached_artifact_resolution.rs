//! Closed materialization lease sources; no opaque factory destructor owns cleanup.
use super::*;

pub(super) enum Source<'a> {
    State(&'a SharedState),
    #[cfg(test)]
    Direct,
}

impl Source<'_> {
    fn acquire_if_present(
        &self,
        artifact_dir: &Path,
        key: &str,
    ) -> std::io::Result<Option<StagedMaterializationGuard>> {
        match self {
            Self::State(state) => {
                acquire_staged_materialization_guard_if_present_for_state(state, artifact_dir, key)
            }
            #[cfg(test)]
            Self::Direct => acquire_staged_materialization_guard_if_present(artifact_dir, key),
        }
    }

    fn acquire(&self, artifact_dir: &Path) -> std::io::Result<StagedMaterializationGuard> {
        match self {
            Self::State(state) => {
                acquire_staged_materialization_guard_for_state(state, artifact_dir)
            }
            #[cfg(test)]
            Self::Direct => acquire_staged_materialization_guard_for_cached_path(artifact_dir),
        }
    }
}

pub(super) fn resolve<'a>(
    cached: &'a CachedArtifact,
    artifact_dir: &Path,
    key_hex: &str,
    source: Source<'_>,
) -> MaterializationResult<MaterializationPayloads<'a>> {
    let existing = cached.payloads.get().cloned();
    let existing_is_staged = existing.as_ref().is_some_and(|payloads| {
        payloads.iter().any(
            |payload| matches!(payload, CachedPayload::File(path) if is_staged_artifact_path(path)),
        )
    });
    let mut staged_guard = if existing_is_staged {
        Some(
            source
                .acquire(artifact_dir)
                .map_err(|error| cache_read_failure(artifact_dir, error))?,
        )
    } else if existing.is_none() && staged_artifacts_enabled() {
        source
            .acquire_if_present(artifact_dir, key_hex)
            .map_err(|error| cache_read_failure(artifact_dir, error))?
    } else {
        None
    };

    let resolved = if let Some(payloads) = existing {
        Some(payloads)
    } else {
        ensure_payloads_with_staged_policy_result(
            cached,
            artifact_dir,
            key_hex,
            staged_guard.is_some(),
        )
        .map_err(|error| cache_read_failure(artifact_dir, error))?
    };

    match resolved {
        Some(payloads) => {
            let is_staged = payloads.iter().any(
                |payload| matches!(payload, CachedPayload::File(path) if is_staged_artifact_path(path)),
            );
            if is_staged {
                // Match ownership explicitly: only the empty branch can acquire
                // a new lease. An existing reader is moved straight back out.
                staged_guard = Some(match staged_guard {
                    Some(guard) => guard,
                    None => source
                        .acquire(artifact_dir)
                        .map_err(|error| cache_read_failure(artifact_dir, error))?,
                });
            } else {
                // A pointer may have disappeared before the shared lock was
                // acquired, causing the resolver to fall back to pack/v1.
                // Non-staged payloads must not retain the staged-store lock.
                staged_guard = None;
            }
            Ok(MaterializationPayloads {
                payloads,
                staged_guard,
                cached,
            })
        }
        None => Err(cache_blob_missing(
            &artifact_dir.join(key_hex),
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "cached artifact metadata or payload is unavailable",
            ),
        )),
    }
}
