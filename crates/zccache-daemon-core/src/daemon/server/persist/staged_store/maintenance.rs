//! Disk-budget scanning and eviction for staged v2 artifact generations.

use super::{
    is_staged_link_or_reparse, pointer_path, staged_key_supported, staged_root,
    validate_staged_artifact_root, StagedCommitGuard, STORE_LOCK,
};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::time::SystemTime;

// A pointer is control metadata (one digest), never an artifact payload.
// Bound both allocation and loop work while revalidating under exclusion.
// The deadline cannot interrupt a single kernel filesystem syscall.
fn read_generation_pointer_for(path: &Path, bound: std::time::Duration) -> io::Result<String> {
    let started = std::time::Instant::now();
    let mut file = fs::File::open(path)?;
    let mut bytes = [0u8; 129];
    let mut length = 0;
    while length < bytes.len() {
        if started.elapsed() >= bound {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "staged pointer read expired",
            ));
        }
        match file.read(&mut bytes[length..]) {
            Ok(0) => break,
            Ok(read) => length += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    if length > 128 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversized staged pointer",
        ));
    }
    let value = std::str::from_utf8(&bytes[..length])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(value.trim().to_owned())
}

pub(crate) struct StagedDiskArtifact {
    pub(crate) key: String,
    #[cfg(test)]
    pub(crate) total_size: u64,
    pub(crate) mtime: SystemTime,
    /// Committed generation observed while holding the shared store lock.
    pub(crate) generation: Option<String>,
}

fn staged_tree_stats(path: &Path) -> io::Result<(u64, SystemTime)> {
    let metadata = fs::symlink_metadata(path)?;
    if is_staged_link_or_reparse(path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing staged artifact scan through linked/reparse path: {}",
                path.display()
            ),
        ));
    }
    let mut size = metadata.len();
    let mut mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    if metadata.is_dir() {
        for entry in fs::read_dir(path)?.flatten() {
            let (child_size, child_mtime) = staged_tree_stats(&entry.path())?;
            size = size.saturating_add(child_size);
            mtime = mtime.max(child_mtime);
        }
    }
    Ok((size, mtime))
}

pub(crate) fn scan_staged_disk_artifacts(
    artifact_dir: &Path,
) -> io::Result<Vec<StagedDiskArtifact>> {
    let root = staged_root(artifact_dir);
    if !validate_staged_artifact_root(artifact_dir)? {
        return Ok(Vec::new());
    }
    // Planning observes names without retaining store exclusion through a
    // recursive scan. Destructive commits revalidate the generation later.
    scan_staged_disk_artifacts_observed(artifact_dir, root.as_path())
}

fn scan_staged_disk_artifacts_observed(
    artifact_dir: &Path,
    root: &Path,
) -> io::Result<Vec<StagedDiskArtifact>> {
    let mut artifacts = Vec::new();
    for entry in fs::read_dir(root)?.flatten() {
        let key_root = entry.path();
        let key_metadata = fs::symlink_metadata(&key_root)?;
        if is_staged_link_or_reparse(&key_root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing staged artifact scan through linked/reparse key root: {}",
                    key_root.display()
                ),
            ));
        }
        if !key_metadata.is_dir() {
            continue;
        }
        let key = entry.file_name().to_string_lossy().into_owned();
        if !staged_key_supported(&key) {
            continue;
        }
        let (mut _total_size, mut mtime) = staged_tree_stats(&key_root)?;
        let pointer = pointer_path(artifact_dir, &key);
        let generation = if let Ok(metadata) = fs::symlink_metadata(&pointer) {
            if is_staged_link_or_reparse(&pointer) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing staged artifact scan through linked/reparse pointer: {}",
                        pointer.display()
                    ),
                ));
            }
            _total_size = _total_size.saturating_add(metadata.len());
            mtime = mtime.max(metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH));
            read_generation_pointer_for(&pointer, crate::core::wait::PROBE)
                .ok()
                .map(|value| value.trim().to_string())
        } else {
            None
        };
        artifacts.push(StagedDiskArtifact {
            key,
            #[cfg(test)]
            total_size: _total_size,
            mtime,
            generation,
        });
    }
    Ok(artifacts)
}

#[cfg(test)]
pub(crate) fn evict_staged_artifact_keys(
    artifact_dir: &Path,
    keys: &HashSet<String>,
) -> io::Result<u64> {
    if keys.is_empty() {
        return Ok(0);
    }
    let root = staged_root(artifact_dir);
    if !validate_staged_artifact_root(artifact_dir)? {
        return Ok(0);
    }
    let _store_lock_guard = StagedCommitGuard::acquire_for(artifact_dir, crate::core::wait::PROBE)?;
    let mut detached = super::trash::DetachedStagedTrash::empty(artifact_dir);
    for key in keys.iter().filter(|key| staged_key_supported(key)) {
        detached.detach(&root.join(key))?;
        detached.detach(&pointer_path(artifact_dir, key))?;
    }
    drop(_store_lock_guard);
    Ok(detached
        .sweep_for(crate::core::wait::PROBE)?
        .reclaimed_bytes)
}

/// Delete only staged generations that still match the maintenance snapshot.
/// The exclusive lock makes compare-and-delete atomic with respect to staged
/// publication without blocking writers during legacy scans or planning.
#[must_use = "release publication exclusion before sweeping the detached trash"]
pub(crate) struct DetachedStagedEviction {
    pub(crate) removed: HashSet<String>,
    pub(crate) trash: super::trash::DetachedStagedTrash,
}

impl DetachedStagedEviction {
    fn empty(artifact_dir: &Path) -> Self {
        Self {
            removed: HashSet::new(),
            trash: super::trash::DetachedStagedTrash::empty(artifact_dir),
        }
    }
}

#[cfg(test)]
pub(crate) fn evict_staged_artifact_keys_if_unchanged(
    artifact_dir: &Path,
    expected: &HashMap<String, Option<String>>,
) -> io::Result<HashSet<String>> {
    let detached = detach_staged_artifact_keys_if_unchanged(
        artifact_dir,
        expected,
        std::time::Duration::from_secs(2),
    )?;
    detached.trash.sweep_for(crate::core::wait::PROBE)?;
    Ok(detached.removed)
}

pub(crate) fn detach_staged_artifact_keys_if_unchanged(
    artifact_dir: &Path,
    expected: &HashMap<String, Option<String>>,
    lock_budget: std::time::Duration,
) -> io::Result<DetachedStagedEviction> {
    let mut detached = DetachedStagedEviction::empty(artifact_dir);
    if expected.is_empty() {
        return Ok(detached);
    }
    let root = staged_root(artifact_dir);
    if !validate_staged_artifact_root(artifact_dir)? {
        return Ok(detached);
    }
    #[cfg(test)]
    super::hook::pause(
        artifact_dir,
        super::StagedHookPoint::MaintenanceStoreLockPending,
    );
    // Maintenance holds publication exclusion here. A foreign warm/reader
    // may retain the store lock indefinitely; defer rather than queue all
    // compiles behind it (soldr#3843 / #3845).
    let _store_lock_guard = match StagedCommitGuard::acquire_for(
        artifact_dir,
        lock_budget.min(std::time::Duration::from_secs(2)),
    ) {
        Ok(guard) => guard,
        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
            tracing::warn!(root = %root.display(), "staged store busy; deferring eviction");
            return Ok(detached);
        }
        Err(error) => return Err(error),
    };
    for (key, expected_generation) in expected.iter().filter(|(key, _)| staged_key_supported(key)) {
        let pointer = pointer_path(artifact_dir, key);
        let current = match fs::symlink_metadata(&pointer) {
            Ok(_) if is_staged_link_or_reparse(&pointer) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing staged artifact eviction through linked/reparse pointer: {}",
                        pointer.display()
                    ),
                ));
            }
            Ok(_) => read_generation_pointer_for(&pointer, crate::core::wait::PROBE)
                .ok()
                .map(|value| value.trim().to_string()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        if &current != expected_generation {
            continue;
        }
        detached.trash.detach(&root.join(key))?;
        detached.trash.detach(&pointer_path(artifact_dir, key))?;
        detached.removed.insert(key.clone());
    }
    Ok(detached)
}

#[cfg(test)]
pub(in crate::daemon::server) fn clear_staged_artifacts(artifact_dir: &Path) -> io::Result<u64> {
    let detached = detach_staged_artifacts_for_clear(artifact_dir, crate::core::wait::PROBE)?;
    Ok(detached
        .sweep_for(crate::core::wait::PROBE)?
        .reclaimed_bytes)
}

pub(in crate::daemon::server) fn detach_staged_artifacts_for_clear(
    artifact_dir: &Path,
    lock_budget: std::time::Duration,
) -> io::Result<super::trash::DetachedStagedTrash> {
    let mut detached = super::trash::DetachedStagedTrash::empty(artifact_dir);
    let root = staged_root(artifact_dir);
    if !validate_staged_artifact_root(artifact_dir)? {
        return Ok(detached);
    }
    #[cfg(test)]
    super::hook::pause(
        artifact_dir,
        super::StagedHookPoint::MaintenanceStoreLockPending,
    );
    let _store_lock_guard =
        StagedCommitGuard::acquire_for(artifact_dir, lock_budget.min(crate::core::wait::PROBE))?;
    let mut children = super::immediate_children::ImmediateChildren::open_for(&root, lock_budget)?;
    while let Some(entry) = children.next_entry()? {
        if entry.file_name() == STORE_LOCK || entry.file_name() == super::trash::STAGED_TRASH {
            continue;
        }
        detached.detach(&entry.path())?;
    }
    Ok(detached)
}

/// Publication has released its shared/key guards before trying this commit.
/// A busy reader defers retirement; the current pointer is checked again so
/// an intervening publication can safely reuse the same immutable generation.
pub(super) fn detach_generation_if_unreferenced_for(
    artifact_dir: &Path,
    key: &str,
    generation: &str,
    lock_budget: std::time::Duration,
) -> io::Result<super::trash::DetachedStagedTrash> {
    super::validate_key(key)?;
    super::validate_generation(generation)?;
    let mut detached = super::trash::DetachedStagedTrash::empty(artifact_dir);
    if !validate_staged_artifact_root(artifact_dir)? {
        return Ok(detached);
    }
    let _guard = StagedCommitGuard::acquire_for(artifact_dir, lock_budget)?;
    match read_generation_pointer_for(&pointer_path(artifact_dir, key), crate::core::wait::PROBE) {
        Ok(current) if current.trim() == generation => return Ok(detached),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    detached.detach(&super::generation_dir(artifact_dir, key, generation))?;
    Ok(detached)
}

/// Scan names without exclusion; revalidate current generations only during
/// the short exclusive commit. No payload tree is walked under the lock.
pub(crate) fn detach_orphaned_staged_paths_for(
    artifact_dir: &Path,
    lock_budget: std::time::Duration,
) -> io::Result<(usize, super::trash::DetachedStagedTrash)> {
    let root = staged_root(artifact_dir);
    let mut detached = super::trash::DetachedStagedTrash::empty(artifact_dir);
    if !validate_staged_artifact_root(artifact_dir)? {
        return Ok((0, detached));
    }
    let mut candidates = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == STORE_LOCK || name == super::trash::STAGED_TRASH {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if is_staged_link_or_reparse(&path) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing orphan scan through linked/reparse key",
            ));
        }
        if metadata.is_file() && name.starts_with('.') {
            candidates.push((path, None));
        } else if metadata.is_dir() && staged_key_supported(&name) {
            for child in fs::read_dir(&path)? {
                let child = child?;
                let child_name = child.file_name().to_string_lossy().into_owned();
                if child_name == super::PUBLISH_LOCK {
                    continue;
                }
                let child_path = child.path();
                let metadata = fs::symlink_metadata(&child_path)?;
                if child_name.starts_with(".tmp-") || metadata.is_dir() {
                    candidates.push((child_path, Some((name.clone(), child_name))));
                }
            }
        }
    }
    if candidates.is_empty() {
        return Ok((0, detached));
    }
    let _guard = StagedCommitGuard::acquire_for(artifact_dir, lock_budget)?;
    let mut removed = 0;
    for (path, generation) in candidates {
        if let Some((key, name)) = generation {
            let current = read_generation_pointer_for(
                &pointer_path(artifact_dir, &key),
                crate::core::wait::PROBE,
            );
            match current {
                Ok(current) if current.trim() == name => continue,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                detached.detach(&path)?;
                removed += 1;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok((removed, detached))
}

#[cfg(test)]
#[path = "sweep_tests.rs"]
mod sweep_tests;

#[cfg(test)]
mod bounded_lock_tests {
    use super::*;

    #[test]
    fn maintenance_contention_defers_eviction_without_waiting_for_the_reader() {
        let directory = tempfile::tempdir().unwrap();
        let root = staged_root(directory.path());
        fs::create_dir_all(root.join("aa")).unwrap();
        fs::write(root.join("aa/payload"), b"preserve while in use").unwrap();
        let reader =
            super::super::StagedReadGuard::acquire_for(directory.path(), crate::core::wait::PROBE)
                .unwrap();
        let expected = HashMap::from([("aa".to_string(), None)]);
        let (tx, rx) = std::sync::mpsc::channel();
        let artifact_dir = directory.path().to_owned();
        let worker_expected = expected.clone();
        let worker = std::thread::spawn(move || {
            let result = evict_staged_artifact_keys_if_unchanged(&artifact_dir, &worker_expected);
            let _ = tx.send(result);
        });
        let deferred = rx.recv_timeout(std::time::Duration::from_secs(10));
        // Always release before joining, including the regression/failure path.
        drop(reader);
        worker.join().unwrap();
        assert!(deferred.unwrap().unwrap().is_empty());
        assert!(root.join("aa/payload").exists());
        assert_eq!(
            evict_staged_artifact_keys_if_unchanged(directory.path(), &expected).unwrap(),
            HashSet::from(["aa".to_string()])
        );
        assert!(!root.join("aa").exists());
    }
}
