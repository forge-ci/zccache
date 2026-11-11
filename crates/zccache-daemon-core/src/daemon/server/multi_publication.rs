//! Multi-source cache metadata replacement and displaced-record ownership.
use super::*;

/// Metadata-only input excludes private staging cleanup during entry lookup.
/// Construct the durable record only at the insertion that takes ownership.
pub(super) fn replace_cached_artifact(
    state: &SharedState,
    key: String,
    meta: ArtifactIndex,
    retired: &mut Vec<CachedArtifact>,
) {
    use dashmap::mapref::entry::Entry;
    match state.artifacts.entry(key) {
        Entry::Occupied(mut entry) => {
            retired.push(entry.insert(CachedArtifact::from_index(meta)));
        }
        Entry::Vacant(entry) => {
            entry.insert(CachedArtifact::from_index(meta));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::server::persist::{StagedHookGuard, StagedHookPoint};
    use std::time::Duration;

    #[tokio::test]
    async fn multi_replacement_releases_publication_before_private_cleanup() {
        crate::test_support::test_timeout(async {
            for unwind in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let server = crate::daemon::server::tests::bind_isolated_server(directory.path());
                let private = server.state.cache_dir.join("multi-private-plan");
                std::fs::create_dir_all(&private).unwrap();
                let output = private.join("old.o");
                std::fs::write(&output, b"private").unwrap();
                let key = "c".repeat(64);
                server.state.artifacts.insert(
                    key.clone(),
                    CachedArtifact::from_provisional_staged(
                        ArtifactIndex::new(vec!["old.o".into()], vec![7], vec![], vec![], 0),
                        vec![output],
                        Arc::new(StagedCompilePlan::for_test(private.clone(), Vec::new())),
                    ),
                );
                let hook = StagedHookGuard::arm(&private, StagedHookPoint::PrivatePlanCleanup);
                let state = Arc::clone(&server.state);
                let replacement_key = key.clone();
                let replacement = tokio::task::spawn_blocking(move || {
                    // Match the production blocking worker's declaration order.
                    let mut retired = Vec::new();
                    let publication = state
                        .artifact_publication
                        .blocking_read_for(Duration::from_secs(5));
                    replace_cached_artifact(
                        &state,
                        replacement_key,
                        ArtifactIndex::new(vec![], vec![], b"replacement".to_vec(), vec![], 0),
                        &mut retired,
                    );
                    assert!(!unwind, "exercise publication worker unwind");
                    drop(publication);
                    drop(retired);
                });
                let hook = tokio::task::spawn_blocking(move || {
                    hook.wait_until_reached_for(Duration::from_secs(10));
                    hook
                })
                .await
                .unwrap();
                let writer = tokio::time::timeout(
                    Duration::from_millis(100),
                    server
                        .state
                        .artifact_publication
                        .drain_for(Duration::from_secs(5)),
                )
                .await;
                let writer_proceeded = writer.is_ok();
                drop(writer);
                let replacement_visible = writer_proceeded
                    && server
                        .state
                        .artifacts
                        .get(&key)
                        .is_some_and(|entry| entry.stdout.as_slice() == b"replacement");
                hook.resume();
                let joined = tokio::time::timeout(Duration::from_secs(10), replacement)
                    .await
                    .unwrap();
                assert_eq!(joined.is_err(), unwind);
                assert!(
                    writer_proceeded,
                    "multi replacement retained publication during private cleanup"
                );
                assert!(
                    replacement_visible,
                    "multi replacement was not visible during private cleanup"
                );
                assert!(!private.exists());
            }
        })
        .await;
    }
}
