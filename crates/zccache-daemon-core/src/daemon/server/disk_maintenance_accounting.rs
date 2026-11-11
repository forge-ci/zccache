//! Physical disk accounting outside publication and store exclusion.

use super::*;
use std::time::Instant;

pub(super) fn add_tree_for(
    root: &Path,
    artifact: &mut DiskArtifact,
    seen: &mut HashSet<crate::platform::fs::FileIdentity>,
    bound: Duration,
) -> io::Result<()> {
    let deadline = Instant::now().checked_add(bound).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "accounting deadline overflow")
    })?;
    add_tree_until(root, artifact, seen, deadline)
}

fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "disk accounting expired",
        ));
    }
    Ok(())
}

fn add_tree_until(
    root: &Path,
    artifact: &mut DiskArtifact,
    seen: &mut HashSet<crate::platform::fs::FileIdentity>,
    deadline: Instant,
) -> io::Result<()> {
    check_deadline(deadline)?;
    if root.is_file() {
        return add_file(root, artifact, seen);
    }
    if !root.is_dir() {
        return Ok(());
    }
    let Some(entries) = listed_entry(root, |entry| std::fs::read_dir(entry))? else {
        return Ok(());
    };
    // Snapshot names before slow per-entry I/O. A continuously publishing
    // process can otherwise keep extending a trash iterator while we walk it.
    // Both enumeration and recursion share one deadline, never a per-child reset.
    // The deadline cannot interrupt an individual kernel filesystem syscall.
    let mut paths = Vec::new();
    for entry in entries {
        check_deadline(deadline)?;
        if let Some(entry) = entry_if_present(entry)? {
            paths.push(entry.path());
        }
    }
    for path in paths {
        check_deadline(deadline)?;
        #[cfg(test)]
        staged_hook_pause(&path, StagedHookPoint::ScanEntryListed);
        check_deadline(deadline)?;
        let Some(metadata) = listed_entry(&path, |entry| std::fs::symlink_metadata(entry))? else {
            continue;
        };
        if crate::platform::fs::links::classify(&path)
            .is_ok_and(|kind| kind != crate::platform::fs::links::LinkKind::Regular)
        {
            continue;
        }
        if metadata.is_dir() {
            add_tree_until(&path, artifact, seen, deadline)?;
        } else if metadata.is_file() {
            add_file(&path, artifact, seen)?;
        }
    }
    check_deadline(deadline)
}
