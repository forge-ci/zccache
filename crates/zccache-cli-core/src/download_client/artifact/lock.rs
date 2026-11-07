use kernal_api::platform::fs::OwnedFileLock;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::download::stable_download_id;

use super::resolve::ResolvedFetchRequest;
use super::WaitMode;

/// A waiter sits behind the owner's entire download, so it gets the same budget
/// as a whole user-requested command rather than a brief state access.
const FETCH_LOCK_WAIT: Duration = zccache_core::wait::WHOLE_COMMAND;

pub(super) struct FetchLock {
    // The lock owns the handle: under `fs2` the open `File` *was* the token,
    // released when it closed. `OwnedFileLock` says that rather than implying
    // it, and still releases on drop.
    _lock: OwnedFileLock,
}

pub(super) fn acquire_fetch_lock(request: &ResolvedFetchRequest) -> Result<FetchLock, String> {
    acquire_lock_at(
        &fetch_lock_path(request),
        request.wait_mode,
        FETCH_LOCK_WAIT,
    )
}

fn acquire_lock_at(
    lock_path: &Path,
    wait_mode: WaitMode,
    bound: Duration,
) -> Result<FetchLock, String> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let open = || {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
    };
    let lock = match wait_mode {
        WaitMode::Block => {
            zccache_core::wait::lock_exclusive_owned_for(open, bound).map_err(|error| {
                if error.kind() == ErrorKind::TimedOut {
                    format!(
                        "timed out after {}s waiting for fetch lock {}: another fetch still owns it",
                        bound.as_secs_f64(),
                        lock_path.display()
                    )
                } else {
                    error.to_string()
                }
            })?
        }
        WaitMode::NoWait => {
            let file = open().map_err(|e| e.to_string())?;
            kernal_api::platform::fs::try_lock_exclusive_owned(file)
                .map_err(|_| "locked".to_string())?
        }
    };
    Ok(FetchLock { _lock: lock })
}

fn fetch_lock_path(request: &ResolvedFetchRequest) -> PathBuf {
    let mut key = crate::core::normalize_for_key(&request.cache_path);
    if let Some(expanded_path) = &request.expanded_path {
        key.push('\n');
        key.push_str(&crate::core::normalize_for_key(expanded_path));
    }
    let hash = stable_download_id(Path::new(&key));
    crate::core::config::daemon_state_dir()
        .join("downloads")
        .join("locks")
        .join(format!("{hash}.lock"))
        .into_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1967: a blocking fetch behind a stuck owner must expire and say why,
    /// and the owner's lock must stay intact and released normally afterwards.
    #[test]
    fn blocking_wait_behind_another_owner_expires_with_a_diagnostic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locks").join("held.lock");
        let bound = Duration::from_millis(50);
        let owner = acquire_lock_at(&path, WaitMode::NoWait, bound).unwrap();

        let message = match acquire_lock_at(&path, WaitMode::Block, bound) {
            Ok(_) => panic!("the waiter must not acquire a held lock"),
            Err(message) => message,
        };
        assert!(message.contains("timed out"), "{message}");
        assert!(message.contains("held.lock"), "{message}");

        drop(owner);
        assert!(acquire_lock_at(&path, WaitMode::Block, bound).is_ok());
    }
}
