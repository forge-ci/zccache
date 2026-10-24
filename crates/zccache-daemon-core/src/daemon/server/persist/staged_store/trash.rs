//! Detached staged trees. Only this owner recursively removes retired data.

use super::{
    is_staged_link_or_reparse, remove_registered_blob, remove_staged_link_or_reparse, staged_root,
    validate_staged_artifact_root, StagedCommitGuard,
};
use crate::core::NormalizedPath;
use std::fs;
use std::io;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

pub(super) const STAGED_TRASH: &str = ".trash";
static NEXT_TRASH: AtomicU64 = AtomicU64::new(1);

/// A capability for paths already renamed out of the live staged namespace.
/// Its fields and constructor stay private to the detach owner. Dropping it
/// leaves durable work for recovery; Drop never performs filesystem cleanup.
#[must_use = "sweep outside exclusion, or explicitly defer to maintenance"]
pub(crate) struct DetachedStagedTrash {
    artifact_dir: NormalizedPath,
    paths: Vec<NormalizedPath>,
}

#[derive(Debug, Default)]
pub(crate) struct StagedSweepReport {
    pub(crate) reclaimed_bytes: u64,
    pub(crate) failures: usize,
    pub(crate) deferred: bool,
}

impl DetachedStagedTrash {
    pub(super) fn empty(artifact_dir: &Path) -> Self {
        Self {
            artifact_dir: NormalizedPath::new(artifact_dir),
            paths: Vec::new(),
        }
    }

    /// Constant-tree-depth destructive commit. Callers own store exclusion.
    pub(super) fn detach(&mut self, path: &Path) -> io::Result<()> {
        let root = staged_root(&self.artifact_dir);
        if !validate_staged_artifact_root(&self.artifact_dir)? {
            return Ok(());
        }
        let relative = path
            .strip_prefix(root.as_path())
            .map_err(|_| invalid_path(path))?;
        if relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
            || relative.as_os_str().is_empty()
            || relative.components().next().is_some_and(|part| {
                part.as_os_str() == STAGED_TRASH || part.as_os_str() == super::STORE_LOCK
            })
        {
            return Err(invalid_path(path));
        }
        self.rename_into_trash(path)
    }

    /// Only the Clear detach owner supplies fixed cache-owned parent paths.
    pub(super) fn detach_cache_child(&mut self, parent: &Path, path: &Path) -> io::Result<()> {
        let root = staged_root(&self.artifact_dir);
        if path.parent() != Some(parent)
            || self.artifact_dir == path
            || path == root.as_path()
            || path.starts_with(root.join(STAGED_TRASH))
            || path == root.join(super::STORE_LOCK).as_path()
        {
            return Err(invalid_path(path));
        }
        self.rename_into_trash(path)
    }

    fn rename_into_trash(&mut self, path: &Path) -> io::Result<()> {
        let root = staged_root(&self.artifact_dir);
        if !validate_staged_artifact_root(&self.artifact_dir)? {
            return Ok(());
        }
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        let trash = root.join(STAGED_TRASH);
        match fs::create_dir(&trash) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        validate_trash(&trash)?;
        let id = NEXT_TRASH.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let destination = trash.join(format!("{}-{nanos}-{id}", std::process::id()));
        // Never overwrite another process's pending trash, even after restart.
        crate::platform::fs::replace::rename_without_replace(path, &destination)?;
        self.paths.push(destination);
        Ok(())
    }

    /// Explicitly retain the durable trash for the next maintenance/restart.
    pub(super) fn defer(self) {}

    /// Run on a blocking worker after publication/store exclusion is released.
    /// The deadline is checked between filesystem operations. It cannot
    /// interrupt an individual kernel syscall; pending paths remain durable.
    pub(crate) fn sweep_for(self, bound: Duration) -> io::Result<StagedSweepReport> {
        self.sweep_until(sweep_deadline(bound)?)
    }

    fn sweep_until(self, deadline: Instant) -> io::Result<StagedSweepReport> {
        let mut report = StagedSweepReport::default();
        if !validate_staged_artifact_root(&self.artifact_dir)? {
            return Ok(report);
        }
        let trash = staged_root(&self.artifact_dir).join(STAGED_TRASH);
        if self.paths.is_empty() {
            if Instant::now() < deadline {
                try_remove_empty_trash(&self.artifact_dir, &trash);
            }
            return Ok(report);
        }
        match validate_trash(&trash) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(report),
            Err(error) => return Err(error),
        }
        for path in self.paths {
            if path.as_path().parent() != Some(trash.as_path()) {
                return Err(invalid_path(&path));
            }
            remove_tree(&path, deadline, &mut report);
            if report.deferred {
                break;
            }
        }
        if !report.deferred && report.failures == 0 && Instant::now() < deadline {
            try_remove_empty_trash(&self.artifact_dir, &trash);
        }
        Ok(report)
    }
}

/// Serialize only the final empty-directory unlink against native detachers.
/// Recursive cleanup has already finished without any store guard. A busy
/// publisher leaves the empty private directory for the next pass.
fn try_remove_empty_trash(artifact_dir: &Path, trash: &Path) {
    let Ok(_guard) = StagedCommitGuard::acquire_for(artifact_dir, Duration::ZERO) else {
        return;
    };
    if validate_trash(trash).is_err() {
        return;
    }
    if let Err(error) = fs::remove_dir(trash) {
        if !matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
        ) {
            tracing::debug!(%error, "empty staged trash directory retained for retry");
        }
    }
}

/// Publication rollback/conflict cleanup only retires names. Maintenance
/// owns the later recursive sweep, after all publication locks are gone.
pub(super) fn defer_staged_tree(artifact_dir: &Path, path: &Path) -> io::Result<()> {
    let mut detached = DetachedStagedTrash::empty(artifact_dir);
    detached.detach(path)?;
    detached.defer();
    Ok(())
}

/// Retry durable trash left by crashes, failed deletes or expired sweeps.
pub(crate) fn sweep_staged_trash_for(
    artifact_dir: &Path,
    bound: Duration,
) -> io::Result<StagedSweepReport> {
    let deadline = sweep_deadline(bound)?;
    if !validate_staged_artifact_root(artifact_dir)? {
        return Ok(StagedSweepReport::default());
    }
    let trash = staged_root(artifact_dir).join(STAGED_TRASH);
    match fs::symlink_metadata(&trash) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(StagedSweepReport::default())
        }
        Err(error) => return Err(error),
        Ok(_) => validate_trash(&trash)?,
    }
    let mut detached = DetachedStagedTrash::empty(artifact_dir);
    let mut enumeration_deferred = false;
    for entry in fs::read_dir(&trash)? {
        if Instant::now() >= deadline {
            enumeration_deferred = true;
            break;
        }
        detached.paths.push(entry?.path().into());
    }
    let mut report = detached.sweep_until(deadline)?;
    report.deferred |= enumeration_deferred;
    Ok(report)
}

fn sweep_deadline(bound: Duration) -> io::Result<Instant> {
    Instant::now().checked_add(bound).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "unrepresentable staged sweep budget",
        )
    })
}

fn validate_trash(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || is_staged_link_or_reparse(path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing staged trash through linked/reparse or non-directory path: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn invalid_path(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "path is outside the live staged detach namespace: {}",
            path.display()
        ),
    )
}

fn failed(path: &Path, error: io::Error, report: &mut StagedSweepReport) {
    if report.failures < 8 {
        tracing::warn!(path = %path.display(), %error, "staged trash cleanup failed; recovery will retry");
    }
    report.failures += 1;
}

fn remove_tree(path: &Path, deadline: Instant, report: &mut StagedSweepReport) {
    if Instant::now() >= deadline {
        report.deferred = true;
        return;
    }
    #[cfg(test)]
    super::hook::pause(path, super::StagedHookPoint::TreeSweep);
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            failed(path, error, report);
            return;
        }
    };
    #[cfg(test)]
    if let Err(error) = super::fault::inject(path, super::StagedFaultPoint::TrashDelete) {
        failed(path, error, report);
        return;
    }
    if is_staged_link_or_reparse(path) {
        if let Err(error) = remove_staged_link_or_reparse(path, &metadata) {
            failed(path, error, report);
        }
        return;
    }
    if metadata.is_dir() {
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) => {
                failed(path, error, report);
                return;
            }
        };
        for entry in entries {
            match entry {
                Ok(entry) => remove_tree(&entry.path(), deadline, report),
                Err(error) => failed(path, error, report),
            }
            if report.deferred {
                return;
            }
        }
        if let Err(error) = fs::remove_dir(path) {
            if error.kind() != io::ErrorKind::NotFound {
                failed(path, error, report);
            }
        }
    } else {
        let allocated = reclaimable_bytes(path, &metadata);
        // Registered removal also unlinks the digest sidecar. Account for it
        // once when it disappears here; later enumeration treats it as absent.
        let digest = super::digest_path(path);
        let digest_bytes = fs::symlink_metadata(&digest)
            .ok()
            .map_or(0, |metadata| reclaimable_bytes(&digest, &metadata));
        match remove_registered_blob(path) {
            Ok(()) => {
                report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(allocated);
                if !digest.exists() {
                    report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(digest_bytes);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => failed(path, error, report),
        }
    }
}

fn reclaimable_bytes(path: &Path, metadata: &fs::Metadata) -> u64 {
    if metadata.is_file() && crate::core::config::file_may_free_space_on_removal(path) {
        crate::platform::fs::volume::allocated_bytes(path, metadata)
    } else {
        0
    }
}
