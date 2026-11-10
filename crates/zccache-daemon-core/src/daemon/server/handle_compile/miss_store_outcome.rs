//! Typed persistence outcomes and bounded publication diagnostics.

use super::{
    write_session_log, ArtifactIndex, IndexWriterCommand, SessionId, SharedState, StagedPublishFailure,
};

pub(super) const MAX_STAGED_PUBLICATION_ERROR_CHARS: usize = 512;

/// Result of the detached single-output persist operation.
///
/// The background task is deliberately required to hand this back to its
/// async parent: an index row is only valid after the artifact files have
/// been published.  Keeping this as a named, must-use outcome prevents a
/// future logging-only error branch from silently turning a failed persist
/// into a durable dangling index entry.
#[derive(Debug)]
#[must_use = "a persist outcome must gate index publication"]
pub(super) struct PersistOutcome {
    published: bool,
    meta: Option<ArtifactIndex>,
}

impl PersistOutcome {
    pub(super) const fn published(meta: ArtifactIndex) -> Self {
        Self {
            published: true,
            meta: Some(meta),
        }
    }

    pub(super) const fn failed() -> Self {
        Self {
            published: false,
            meta: None,
        }
    }
}

/// The only async single-output index-publication gate.
///
/// Keep the outcome consumption next to the channel send: a failed persist
/// must remain a clean miss rather than becoming an index row whose payload
/// never existed.
pub(super) fn enqueue_persisted_index(
    outcome: PersistOutcome,
    index_writer_tx: &kernal_api::async_engine::UnboundedSender<IndexWriterCommand>,
    key_hex: String,
) -> bool {
    if !outcome.published {
        return false;
    }
    let Some(meta) = outcome.meta else {
        return false;
    };
    // #1177: this send only fails when the index writer is gone, which means
    // the daemon has silently stopped recording what it caches. `enqueue`
    // needs the state to bound the report, so the one call site that has only
    // the sender keeps the raw send and its own report.
    if index_writer_tx
        .send(IndexWriterCommand::Insert(key_hex.clone(), meta))
        .is_err()
    {
        tracing::error!(
            artifact_key = %key_hex,
            "index writer is gone; persisted artifact will not survive a daemon restart"
        );
    }
    true
}

/// Record the full persistence failure at the stage where publication is
/// abandoned. The profiler retains the compact reason ID; the lifecycle and
/// session logs retain a bounded OS error so an operator can identify the
/// failed operation and path after the compiler output has been salvaged.
pub(super) fn report_staged_publication_failure(
    state: &SharedState,
    sid: &SessionId,
    artifact_key_hex: &str,
    reason: StagedPublishFailure,
    error: impl std::fmt::Display,
) {
    let error = truncate_staged_publication_error(&error.to_string());
    let reason_id = reason.id();
    tracing::warn!(
        key = %artifact_key_hex,
        reason = reason_id,
        error = %error,
        "staged artifact publication failed"
    );
    crate::core::lifecycle::write_event(
        crate::core::lifecycle::EVENT_STAGED_PUBLICATION_FAILED,
        serde_json::json!({
            "reason": reason_id,
            "error": error,
            "artifact_key": artifact_key_hex,
        }),
    );
    write_session_log(
        &state.sessions,
        sid,
        &format!(
            "[DIAG] staged_publication_failed: reason={reason_id} key={artifact_key_hex} error={error}"
        ),
    );
}

pub(super) fn truncate_staged_publication_error(error: &str) -> String {
    let mut chars = error.chars();
    let truncated: String = chars
        .by_ref()
        .take(MAX_STAGED_PUBLICATION_ERROR_CHARS)
        .collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}
