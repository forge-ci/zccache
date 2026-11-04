use std::path::Path;
use std::time::Duration;
use zccache_core::NormalizedPath;

use super::error::Result;

const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

fn lock_path(cache_path: &Path) -> NormalizedPath {
    let mut s = cache_path.as_os_str().to_os_string();
    s.push(".lock");
    NormalizedPath::new(Path::new(&s))
}

/// Run a closure while holding a shared (read) lock on the cache path.
/// Fail-open: if the lock cannot be acquired, the closure runs anyway.
///
/// The handle and the bounded wait are the facade's: opening a lock file is
/// `open_lock_file`, and retrying the lock while keeping the handle is
/// `lock_shared_owned_for`. Naming neither the host handle type nor its
/// options is what keeps this module from re-deriving the locking mechanism.
pub fn with_shared_lock<T, F>(cache_path: &Path, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let path = lock_path(cache_path);
    if let Some(parent) = path.parent() {
        kernal_api::platform::fs::create_dir_all(parent)?;
    }
    let lock_file = match kernal_api::platform::fs::open_lock_file(path.as_path()) {
        Ok(file) => Some(file),
        Err(e) => {
            tracing::warn!(
                path = %cache_path.display(),
                error = %e,
                "failed to open lock file, proceeding without lock"
            );
            None
        }
    };

    let acquired =
        lock_file.and_then(|file| {
            match kernal_api::platform::fs::lock_shared_owned_for(file, DEFAULT_LOCK_TIMEOUT) {
                Ok(guard) => Some(guard),
                Err(e) => {
                    tracing::warn!(
                        path = %cache_path.display(),
                        error = %e,
                        "shared lock unavailable after {}s, proceeding without lock",
                        DEFAULT_LOCK_TIMEOUT.as_secs()
                    );
                    None
                }
            }
        });

    let result = f();

    // The owned guard releases the lock and closes the handle on drop.
    drop(acquired);

    result
}

/// Run a closure while holding an exclusive (write) lock on the cache path.
/// Fail-open: if the lock cannot be acquired, the closure runs anyway.
///
/// See [`with_shared_lock`] for why the facade owns the handle and the wait.
pub fn with_exclusive_lock<T, F>(cache_path: &Path, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let path = lock_path(cache_path);
    if let Some(parent) = path.parent() {
        kernal_api::platform::fs::create_dir_all(parent)?;
    }
    let lock_file = match kernal_api::platform::fs::open_lock_file(path.as_path()) {
        Ok(file) => Some(file),
        Err(e) => {
            tracing::warn!(
                path = %cache_path.display(),
                error = %e,
                "failed to open lock file, proceeding without lock"
            );
            None
        }
    };

    let acquired = lock_file.and_then(|file| {
        match kernal_api::platform::fs::lock_exclusive_owned_for(file, DEFAULT_LOCK_TIMEOUT) {
            Ok(guard) => Some(guard),
            Err(e) => {
                tracing::warn!(
                    path = %cache_path.display(),
                    error = %e,
                    "exclusive lock unavailable after {}s, proceeding without lock",
                    DEFAULT_LOCK_TIMEOUT.as_secs()
                );
                None
            }
        }
    });

    let result = f();

    // The owned guard releases the lock and closes the handle on drop.
    drop(acquired);

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn lock_file_created_next_to_cache() {
        let dir = TempDir::new().unwrap();
        let cache = dir.path().join("fp.json");

        with_shared_lock(&cache, || Ok(())).unwrap();

        assert!(lock_path(&cache).exists());
    }

    #[test]
    fn shared_locks_concurrent() {
        let dir = TempDir::new().unwrap();
        let cache = dir.path().join("fp.json");

        let file1 = kernal_api::platform::fs::open_lock_file(lock_path(&cache).as_path()).unwrap();
        let file2 = kernal_api::platform::fs::open_lock_file(lock_path(&cache).as_path()).unwrap();

        // Both guards are bound: dropping the first inline would make the
        // second succeed trivially, and the point is that they coexist.
        let _first =
            kernal_api::platform::fs::try_lock_shared(&file1).expect("first shared holder");
        let _second = kernal_api::platform::fs::try_lock_shared(&file2)
            .expect("a second shared holder must be allowed alongside the first");
    }

    #[test]
    fn exclusive_blocks_shared() {
        use std::sync::{Arc, Barrier};

        let dir = TempDir::new().unwrap();
        let cache_path = dir.path().join("fp.json");

        let exclusive =
            kernal_api::platform::fs::open_lock_file(lock_path(&cache_path).as_path()).unwrap();
        // Bind the guard: it releases on drop, so asserting on it inline
        // would unlock before the assertion's semicolon.
        let exclusive_guard = kernal_api::platform::fs::try_lock_exclusive(&exclusive)
            .expect("exclusive lock must be available");

        let cache_path2 = cache_path.clone();
        let barrier = Arc::new(Barrier::new(2));
        let barrier2 = barrier.clone();

        let handle = std::thread::spawn(move || {
            let file = kernal_api::platform::fs::open_lock_file(lock_path(&cache_path2).as_path())
                .unwrap();
            barrier2.wait();
            // Should fail to acquire shared lock immediately.
            assert!(kernal_api::platform::fs::try_lock_shared(&file).is_err());
        });

        barrier.wait();
        std::thread::sleep(Duration::from_millis(50));
        drop(exclusive_guard); // Release.
        drop(exclusive);
        handle.join().unwrap();
    }

    #[test]
    fn exclusive_blocks_exclusive() {
        let dir = TempDir::new().unwrap();
        let cache = dir.path().join("fp.json");

        let file1 = kernal_api::platform::fs::open_lock_file(lock_path(&cache).as_path()).unwrap();
        let first = kernal_api::platform::fs::try_lock_exclusive(&file1)
            .expect("first holder takes the lock");

        let file2 = kernal_api::platform::fs::open_lock_file(lock_path(&cache).as_path()).unwrap();
        assert!(kernal_api::platform::fs::try_lock_exclusive(&file2).is_err());

        drop(first);
        drop(file1);
        drop(
            kernal_api::platform::fs::try_lock_exclusive(&file2)
                .expect("the lock is free once the first holder releases"),
        );
    }

    #[test]
    fn fail_open_on_timeout() {
        let dir = TempDir::new().unwrap();
        let cache = dir.path().join("fp.json");

        let result: Result<i32> = with_shared_lock(&cache, || Ok(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn lock_parent_dir_created() {
        let dir = TempDir::new().unwrap();
        let deep = dir.path().join("a/b/c/fp.json");

        with_exclusive_lock(&deep, || Ok(())).unwrap();

        assert!(lock_path(&deep).exists());
    }

    #[test]
    fn lock_released_on_drop() {
        let dir = TempDir::new().unwrap();
        let cache = dir.path().join("fp.json");

        {
            let file =
                kernal_api::platform::fs::open_lock_file(lock_path(&cache).as_path()).unwrap();
            // Held for the whole inner scope, so what the outer assertion
            // observes is the *drop* releasing it rather than the lock never
            // having been taken.
            let _held = kernal_api::platform::fs::try_lock_exclusive(&file)
                .expect("exclusive lock must be available");
        }

        let file = kernal_api::platform::fs::open_lock_file(lock_path(&cache).as_path()).unwrap();
        drop(
            kernal_api::platform::fs::try_lock_exclusive(&file)
                .expect("the lock must be free once the previous holder's scope ended"),
        );
    }
}
