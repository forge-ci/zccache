//! Commit generic-tool metadata after successful requested-output delivery.
use super::*;

pub(super) fn finish_staged_exec_publication(
    state: &SharedState,
    key: &str,
    cached: CachedArtifact,
    publication_guard: Option<crate::core::sync::PublicationReadPermit>,
) -> impl std::future::Future<Output = ()> {
    // Keep retired owners outside the permit's destruction scope on unwind.
    let retired;
    let publication = publication_guard;
    retired = state.artifacts.insert(key.to_string(), cached);
    drop(publication);
    super::cached_artifact_retirement::retire_for(
        retired.into_iter().collect(),
        crate::core::sync::OPERATION_TIMEOUT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::server::persist::{StagedHookGuard, StagedHookPoint};
    use std::time::Duration;

    #[tokio::test]
    async fn exec_replacement_releases_publication_before_private_cleanup() {
        crate::test_support::test_timeout(async {
            let directory = tempfile::tempdir().unwrap();
            let server = crate::daemon::server::tests::bind_isolated_server(directory.path());
            let private = server.state.cache_dir.join("exec-private-plan");
            std::fs::create_dir_all(&private).unwrap();
            let output = private.join("old.out");
            std::fs::write(&output, b"private").unwrap();
            let key = "c".repeat(64);
            server.state.artifacts.insert(
                key.clone(),
                CachedArtifact::from_provisional_staged(
                    ArtifactIndex::new(vec!["old.out".into()], vec![7], vec![], vec![], 0),
                    vec![output],
                    Arc::new(StagedCompilePlan::for_test(private.clone(), Vec::new())),
                ),
            );
            let hook = StagedHookGuard::arm(&private, StagedHookPoint::PrivatePlanCleanup);
            let state = Arc::clone(&server.state);
            let replacement_key = key.clone();
            let replacement = tokio::task::spawn_blocking(move || {
                let publication = state
                    .artifact_publication
                    .blocking_read_for(Duration::from_secs(5));
                let artifact = ArtifactData {
                    outputs: Vec::new(),
                    stdout: Arc::new(b"replacement".to_vec()),
                    stderr: Arc::new(Vec::new()),
                    exit_code: 0,
                };
                // The production helper must schedule retirement before returning
                // a waiter, so dropping an unpolled waiter cannot cancel cleanup.
                drop(finish_staged_exec_publication(
                    &state,
                    &replacement_key,
                    CachedArtifact::from_artifact_data(&artifact),
                    Some(publication),
                ));
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
            tokio::time::timeout(Duration::from_secs(10), replacement)
                .await
                .unwrap()
                .unwrap();
            // The waiter was deliberately dropped; a FIFO blocking task is not
            // a join proof. Observe actual filesystem completion with a budget.
            tokio::time::timeout(Duration::from_secs(5), async {
                while private.exists() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            assert!(
                writer_proceeded,
                "exec replacement retained publication during private cleanup"
            );
            assert!(
                replacement_visible,
                "exec replacement was not visible during cleanup"
            );
        })
        .await;
    }
}

#[cfg(test)]
#[path = "exec_direct_retirement_tests.rs"]
mod direct_retirement_tests;
