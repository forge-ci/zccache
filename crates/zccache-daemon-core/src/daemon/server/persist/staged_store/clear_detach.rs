//! Clear's short disk-name retirement phase; no recursive deletion lives here.
use super::*;
use std::time::Duration;

pub(in crate::daemon::server) fn detach_clear_artifact_paths_for(
    artifact_dir: &Path,
    cache_dir: &NormalizedPath,
    bound: Duration,
) -> io::Result<DetachedStagedTrash> {
    let mut detached = detach_staged_artifacts_for_clear(artifact_dir, bound)?;
    let root = staged_root(artifact_dir);
    // Also supplies an owned trash root for a legacy-only cache.
    let _guard = StagedReadGuard::acquire_for(artifact_dir, bound)?;
    let mut children = super::immediate_children::ImmediateChildren::open_for(artifact_dir, bound)?;
    while let Some(entry) = children.next_entry()? {
        let path = entry.path();
        if path == root.as_path() {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_file() || is_staged_link_or_reparse(&path) {
            detached.detach_cache_child(artifact_dir, &path)?;
        }
    }
    // Retire snapshots while publication is still excluded. Deleting their
    // live names after releasing the writer could erase a fresh checkpoint.
    let metadata = crate::core::config::metadata_path_from_cache_dir(cache_dir);
    let depgraph = crate::daemon::server::depgraph_file_path_for_cache_dir(cache_dir);
    for path in [metadata, depgraph] {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot has no owner directory",
            )
        })?;
        detached.detach_cache_child(parent, path.as_path())?;
    }
    // Preserve the prior exact meson-cache location beneath this daemon store.
    if let Some(parent) = artifact_dir.parent() {
        detached.detach_cache_child(parent, &parent.join("meson-configure"))?;
    }
    Ok(detached)
}
