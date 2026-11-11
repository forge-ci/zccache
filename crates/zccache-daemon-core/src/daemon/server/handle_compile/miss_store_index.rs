//! Identity-safe provisional removal and durable Rust verdict merging.
use super::{
    enqueue_index_insert, send_staged_index_insert, ArtifactIndex, CachedArtifact, SharedState,
    StagedPublishFailure,
};

pub(super) fn remove_provisional_artifact(
    state: &SharedState,
    key_hex: &str,
    provisional: &CachedArtifact,
) {
    if let dashmap::mapref::entry::Entry::Occupied(entry) =
        state.artifacts.entry(key_hex.to_owned())
    {
        if entry.get().same_instance(provisional) {
            entry.remove();
        }
    }
}

pub(super) fn commit_rustc_artifact_index(
    state: &SharedState,
    key_hex: String,
    mut meta: ArtifactIndex,
    staged: bool,
    retired: &mut Vec<CachedArtifact>,
) -> Result<u64, StagedPublishFailure> {
    use dashmap::mapref::entry::Entry;

    if let Some(durable) = super::super::rustc_index::durable_rustc_index(state, &key_hex) {
        meta = super::super::rustc_index::merge_rustc_index(meta, durable);
    }

    let commit = |metadata: ArtifactIndex| {
        if staged {
            send_staged_index_insert(state, key_hex.clone(), metadata)
        } else {
            enqueue_index_insert(state, key_hex.clone(), metadata);
            Ok(0)
        }
    };

    match state.artifacts.entry(key_hex.clone()) {
        Entry::Occupied(mut entry) => {
            meta = super::super::rustc_index::merge_rustc_index(meta, entry.get().meta.clone());
            let commit_ns = commit(meta.clone())?;
            retired.push(entry.insert(CachedArtifact::from_index(meta)));
            Ok(commit_ns)
        }
        Entry::Vacant(entry) => {
            let commit_ns = commit(meta.clone())?;
            entry.insert(CachedArtifact::from_index(meta));
            Ok(commit_ns)
        }
    }
}
