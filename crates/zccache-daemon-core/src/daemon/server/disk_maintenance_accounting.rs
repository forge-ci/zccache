//! Physical disk accounting outside publication and store exclusion.

use super::*;

pub(super) fn add_tree(
    root: &Path,
    artifact: &mut DiskArtifact,
    seen: &mut HashSet<crate::platform::fs::FileIdentity>,
) -> io::Result<()> {
    if root.is_file() {
        return add_file(root, artifact, seen);
    }
    if !root.is_dir() {
        return Ok(());
    }
    let Some(entries) = listed_entry(root, |entry| std::fs::read_dir(entry))? else {
        return Ok(());
    };
    for entry in entries {
        let Some(entry) = entry_if_present(entry)? else {
            continue;
        };
        let path = entry.path();
        #[cfg(test)]
        staged_hook_pause(&path, StagedHookPoint::ScanEntryListed);
        let Some(metadata) = listed_entry(&path, |entry| std::fs::symlink_metadata(entry))? else {
            continue;
        };
        if crate::platform::fs::links::classify(&path)
            .is_ok_and(|kind| kind != crate::platform::fs::links::LinkKind::Regular)
        {
            continue;
        }
        if metadata.is_dir() {
            add_tree(&path, artifact, seen)?;
        } else if metadata.is_file() {
            add_file(&path, artifact, seen)?;
        }
    }
    Ok(())
}

