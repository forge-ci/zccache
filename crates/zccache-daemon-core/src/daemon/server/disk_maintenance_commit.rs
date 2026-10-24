//! Short maintenance commit; recursive cleanup is returned to the caller.
use super::*;

pub(super) struct RemovalCommit {
    pub(super) keys: Vec<String>,
    pub(super) trash: DetachedStagedTrash,
    pub(super) legacy_reclaimed_bytes: u64,
}

pub(super) struct RemovalSelection<'a> {
    pub(super) scanned: &'a [DiskArtifact],
    pub(super) plan: &'a MaintenancePlan,
}

pub(super) fn remove_planned_artifacts(
    artifact_dir: &Path,
    selection: RemovalSelection<'_>,
    artifacts: &DashMap<String, CachedArtifact>,
    artifact_store: &ArtifactStore,
    index_writer_tx: Option<&kernal_api::async_engine::UnboundedSender<IndexWriterCommand>>,
    dep_graph: &DepGraph,
    retired_artifacts: &mut Vec<CachedArtifact>,
) -> io::Result<RemovalCommit> {
    let RemovalSelection { scanned, plan } = selection;
    let planned: HashSet<&str> = plan.selected.iter().map(String::as_str).collect();
    let staged_expected: HashMap<String, Option<String>> = scanned
        .iter()
        .filter(|artifact| artifact.staged && planned.contains(artifact.key.as_str()))
        .map(|artifact| (artifact.key.clone(), artifact.staged_generation.clone()))
        .collect();
    let detached = evict_v2_artifact_keys_if_unchanged(artifact_dir, &staged_expected)?;
    let staged_removed = &detached.removed;
    let mut legacy_reclaimed_bytes = 0_u64;
    let selected: HashSet<&str> = scanned
        .iter()
        .filter(|artifact| {
            planned.contains(artifact.key.as_str())
                && (!artifact.staged || staged_removed.contains(&artifact.key))
        })
        .map(|artifact| artifact.key.as_str())
        .collect();
    for artifact in scanned
        .iter()
        .filter(|artifact| selected.contains(artifact.key.as_str()))
    {
        for file in &artifact.legacy_files {
            let reclaimable = std::fs::symlink_metadata(file).ok().map_or(0, |metadata| {
                if crate::core::config::file_may_free_space_on_removal(file) {
                    crate::platform::fs::volume::allocated_bytes(file, &metadata)
                } else {
                    0
                }
            });
            match remove_cow_blob(file) {
                Ok(()) => {
                    legacy_reclaimed_bytes = legacy_reclaimed_bytes.saturating_add(reclaimable)
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    let removed_keys: Vec<String> = plan
        .selected
        .iter()
        .filter(|key| selected.contains(key.as_str()))
        .cloned()
        .collect();
    if removed_keys.is_empty() {
        return Ok(RemovalCommit {
            keys: removed_keys,
            trash: detached.trash,
            legacy_reclaimed_bytes,
        });
    }

    let keys: Vec<&str> = removed_keys.iter().map(String::as_str).collect();
    // The caller owns the publication barrier's write side. Every cache hit
    // that acquired an owned read lease has therefore finished payload
    // discovery and enqueued its access checkpoint; the subsequent Remove is
    // ordered after every such Insert. New lookups cannot acquire a lease and
    // rehydrate the entry from ArtifactStore in the gap.
    artifact_store.remove_batch(&keys);
    dep_graph.invalidate_artifact_keys(&removed_keys.iter().cloned().collect());
    for key in &removed_keys {
        if let Some((_, artifact)) = artifacts.remove(key) {
            retired_artifacts.push(artifact);
        }
    }
    if let Some(tx) = index_writer_tx {
        let _ = tx.send(IndexWriterCommand::Remove(removed_keys.clone()));
    }
    Ok(RemovalCommit {
        keys: removed_keys,
        trash: detached.trash,
        legacy_reclaimed_bytes,
    })
}
