//! Stat-level file identity that `touch`/`utimensat` cannot forge (#1943).
//!
//! `(mtime, size)` alone cannot see an in-place edit whose size is unchanged
//! and whose mtime was put back (`sed -i` + `touch -d`, rebases, archive
//! tools). On Unix the inode change time (`ctime`) advances on every content
//! write and cannot be set from userspace, and the inode number catches a
//! replace-by-rename. Both come from the `stat` the cache already performs,
//! so the check costs zero extra syscalls. Windows exposes neither through
//! stable `std::fs::Metadata`; there both fields are `0` and detection falls
//! back to the watcher, matching `daemon/fingerprint/verify.rs` (#1897).

use serde::{Deserialize, Serialize};

/// `(ctime_ns, inode)` observed alongside `(mtime, size)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileIdentity {
    /// Inode change time in nanoseconds since the epoch (`0` = unavailable).
    pub change_ns: u64,
    /// Inode number (`0` = unavailable).
    pub file_id: u64,
}

impl FileIdentity {
    /// Read the identity from metadata the caller already holds.
    #[must_use]
    pub fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        let (change_ns, file_id) = zccache_ipc::metadata_identity(metadata);
        Self { change_ns, file_id }
    }
}
