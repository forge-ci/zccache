//! Exclusive cache-root writer ownership shared by daemons and offline operations.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Lock file naming the single live writer of a cache root (#1162).
const CACHE_ROOT_WRITER_LOCK_FILE: &str = ".writer.lock";
/// Exclusive claim on a cache root, held for as long as the daemon writes to
/// it (#1162 finding 1).
///
/// `ArtifactStore::flush` serializes the whole in-memory index and atomically
/// renames it over `index.bin` — no merge, no read-modify-write. Two writers on
/// one root therefore do not interleave, they *overwrite*: each flush discards
/// everything the other inserted. The blobs survive on disk but become
/// unreferenced, so the damage shows up much later as unexplained cold misses.
///
/// Nothing else excludes them. The embedded service uses a synthetic
/// `embedded:` endpoint and never binds IPC, so the IPC singleton lockfile does
/// not stop a standalone daemon from opening the same root — one stray
/// `zccache` compile against `ZCCACHE_CACHE_DIR=X` is enough.
///
/// Contention is a misconfiguration worth surfacing, not papering over, so
/// acquisition is `try_lock` and a loser refuses to start rather than silently
/// coexisting.
/// Release is explicit rather than `Drop`-driven because `Arc<SharedState>`
/// outlives daemon shutdown: background holders (index writer, maintenance,
/// loaders) keep clones alive after the server task has joined. Waiting for the
/// last `Arc` would hold the root long past the point where the daemon stopped
/// writing, and a sequential restart on the same root — which is legitimate,
/// and which the integration suite does — would be refused. `Drop` remains as
/// the crash backstop.
/// Background filesystem jobs use separate [`CacheRootWriterLease`] values so
/// they keep the native claim through actual completion after server release.
#[derive(Debug)]
pub struct CacheRootWriterLock {
    lock: crate::blocking_sync::Mutex<Option<Arc<kernal_api::platform::fs::OwnedFileLock>>>,
}

/// Data-free root ownership for one background filesystem job.
///
/// Release of the server's claim closes admission for new leases. Existing
/// jobs retain native exclusion until they finish and drop their leases.
#[derive(Debug)]
#[must_use = "retain root ownership through the background filesystem job"]
pub struct CacheRootWriterLease {
    _lock: Arc<kernal_api::platform::fs::OwnedFileLock>,
}

impl CacheRootWriterLock {
    /// Claim `cache_dir` for this process, or report who already holds it.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::ErrorKind::WouldBlock`] when another live writer
    /// holds the root; `lifecycle::cache_root_error` preserves that kind, so
    /// callers can tell contention from a genuine filesystem fault.
    pub fn acquire(cache_dir: &Path) -> std::io::Result<Self> {
        use std::io::Write;

        std::fs::create_dir_all(cache_dir)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(cache_dir.join(CACHE_ROOT_WRITER_LOCK_FILE))?;
        let Ok(lock) = kernal_api::platform::fs::try_lock_exclusive_owned(file) else {
            crate::lifecycle::write_event_in_cache_root(
                cache_dir,
                "daemon_cache_root_contended",
                serde_json::json!({
                    "cache_root": cache_dir.display().to_string(),
                    "pid": std::process::id(),
                }),
            );
            tracing::warn!(
                cache_root = %cache_dir.display(),
                pid = std::process::id(),
                "cache root already has a live writer; refusing to start a second one"
            );
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "another live daemon already holds this cache root as its writer",
            ));
        };
        // Best-effort provenance for whoever inspects the lock file; the lock
        // itself is what enforces exclusion, so a failed write is not fatal.
        let _ = lock.file().set_len(0);
        let _ = writeln!(lock.file(), "{}", std::process::id());
        // #1673: stamp the store as in use on daemon start so an older-version
        // daemon's retired-store sweep leaves it alone. Never fails acquire.
        if let Err(error) = crate::config::touch_store_activity_marker(cache_dir) {
            tracing::debug!(%error, cache_root = %cache_dir.display(), "failed to stamp store activity marker");
        }
        Ok(Self {
            lock: crate::blocking_sync::Mutex::new(Some(Arc::new(lock))),
        })
    }

    /// Retain an active writer claim within an explicit acquisition budget.
    /// Returns `None` after shutdown has released the server's claim.
    pub fn lease_for(&self, bound: Duration) -> Option<CacheRootWriterLease> {
        self.lock
            .lock_for(bound)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|lock| CacheRootWriterLease {
                _lock: Arc::clone(lock),
            })
    }

    /// Close new job admission and release the server's claim.
    ///
    /// Call this once the daemon has finished its shutdown persistence — that
    /// is the moment it stops writing, which is what the claim actually
    /// guards. Idempotent, so the `Drop` backstop after an explicit release is
    /// a no-op. Outstanding job leases retain the native claim until their
    /// filesystem work finishes; new leases are refused after this call.
    pub fn release(&self) {
        let lock = {
            let mut guard = self
                .lock
                .lock_for(crate::sync::STATE_TIMEOUT)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.take()
        };
        drop(lock);
    }
}

impl Drop for CacheRootWriterLock {
    fn drop(&mut self) {
        self.release();
    }
}
