//! Exact-root validation and safe link/reparse removal for staged artifacts.

use std::fs;
use std::io;
use std::path::Path;

// Root naming, root validation and lock-file opening are re-exported from
// `zccache-artifact` rather than reimplemented here, so the daemon and the
// in-process `zccache warm` command provably contend on one lock file.
// See `zccache_artifact::staged_lock`.
pub(super) use zccache_artifact::staged_lock::{
    is_staged_link_or_reparse, staged_root, validate_staged_root_path, StagedCommitGuard,
    StagedReadGuard,
};

pub(super) fn remove_staged_link_or_reparse(
    path: &Path,
    metadata: &fs::Metadata,
) -> io::Result<()> {
    use crate::platform::fs::links::LinkKind;
    if crate::platform::fs::links::classify(path)? != LinkKind::Regular {
        // A directory-flavoured link (a Windows junction or directory
        // symlink) needs `remove_dir`, but its no-follow file type is not a
        // directory, so try that first; every other link is a file.
        if metadata.is_dir() {
            return fs::remove_dir(path);
        }
        fs::remove_dir(path).or_else(|_| fs::remove_file(path))
    } else {
        fs::remove_file(path)
    }
}

pub(in crate::daemon::server) fn validate_staged_artifact_root(
    artifact_dir: &Path,
) -> io::Result<bool> {
    validate_staged_root_path(staged_root(artifact_dir).as_path())
}
