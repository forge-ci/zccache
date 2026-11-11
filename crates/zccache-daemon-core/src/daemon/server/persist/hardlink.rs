//! Cross-platform hardlink helpers: hardlink-detach (write-without-mutating-cache)
//! and the failure-injection seams its tests use. File identity, link
//! counts, and permission mechanics live behind `crate::platform`.

use super::*;

#[cfg(test)]
static FAIL_DETACH_REMOVE_PATHS: std::sync::OnceLock<
    zccache_core::blocking_sync::Mutex<std::collections::HashSet<PathBuf>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
static FAIL_DETACH_RENAME_PATHS: std::sync::OnceLock<
    zccache_core::blocking_sync::Mutex<std::collections::HashSet<PathBuf>>,
> = std::sync::OnceLock::new();

pub(in crate::daemon::server) fn remove_output_file(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if let Ok(mut injected) = FAIL_DETACH_REMOVE_PATHS
        .get_or_init(|| zccache_core::blocking_sync::Mutex::new(std::collections::HashSet::new()))
        .lock_for(zccache_core::sync::STATE_TIMEOUT)
    {
        if injected.remove(path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected detach remove failure",
            ));
        }
    }
    std::fs::remove_file(path)
}

pub(in crate::daemon::server) fn remove_output_file_scoped(
    destination: &OutputDestination,
) -> std::io::Result<()> {
    #[cfg(test)]
    let path = destination.as_ref();
    #[cfg(test)]
    if let Ok(mut injected) = FAIL_DETACH_REMOVE_PATHS
        .get_or_init(|| zccache_core::blocking_sync::Mutex::new(std::collections::HashSet::new()))
        .lock_for(zccache_core::sync::STATE_TIMEOUT)
    {
        if injected.remove(path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected detach remove failure",
            ));
        }
    }
    destination.unlink()
}

fn rename_detached_output(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if let Ok(mut injected) = FAIL_DETACH_RENAME_PATHS
        .get_or_init(|| zccache_core::blocking_sync::Mutex::new(std::collections::HashSet::new()))
        .lock_for(zccache_core::sync::STATE_TIMEOUT)
    {
        if injected.remove(to) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected detach rename failure",
            ));
        }
    }
    std::fs::rename(from, to)
}

#[cfg(test)]
pub(in crate::daemon::server) fn fail_detach_remove_for_test(path: &Path) {
    FAIL_DETACH_REMOVE_PATHS
        .get_or_init(|| zccache_core::blocking_sync::Mutex::new(std::collections::HashSet::new()))
        .lock_for(zccache_core::sync::STATE_TIMEOUT)
        .expect("detach failure injection lock")
        .insert(path.to_path_buf());
}

#[cfg(test)]
pub(in crate::daemon::server) fn fail_detach_rename_for_test(path: &Path) {
    FAIL_DETACH_RENAME_PATHS
        .get_or_init(|| zccache_core::blocking_sync::Mutex::new(std::collections::HashSet::new()))
        .lock_for(zccache_core::sync::STATE_TIMEOUT)
        .expect("detach rename failure injection lock")
        .insert(path.to_path_buf());
}

pub(in crate::daemon::server) fn break_output_hardlink_before_compile(
    path: &Path,
) -> std::io::Result<()> {
    break_output_hardlink_with(path, || remove_output_file(path))
}

/// Detach only the compiler-visible output authorized by the caller's scope.
pub(super) fn break_output_hardlink_before_compile_scoped(
    destination: &super::OutputDestination,
) -> std::io::Result<()> {
    break_output_hardlink_with(destination.as_ref(), || {
        remove_output_file_scoped(destination)
    })
}

fn break_output_hardlink_with(
    path: &Path,
    remove: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    if crate::platform::fs::links::hard_link_count(path)? <= 1 {
        return crate::platform::fs::permissions::make_writable(path);
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut source = std::fs::File::open(path)?;
    std::io::copy(&mut source, temporary.as_file_mut())?;
    // The next compiler invocation rewrites this independent build output.
    // Only the rename must be atomic; the transient copy needs no fsync.
    let permissions = source.metadata()?.permissions();
    std::fs::set_permissions(temporary.path(), permissions)?;
    drop(source);
    let temporary = temporary.into_temp_path();
    let registration = prepare_registered_detach(path);
    crate::platform::fs::permissions::make_writable(path)?;
    if let Err(error) = remove() {
        if let Some((_, blob_path)) = &registration {
            let _ = crate::platform::fs::permissions::set_cache_blob_sealed(
                blob_path,
                readonly_enabled(),
            );
        }
        return Err(error);
    }
    if let Err(error) = rename_detached_output(&temporary, path) {
        if let Some((id, blob_path)) = &registration {
            let _ = crate::platform::fs::permissions::set_cache_blob_sealed(
                blob_path,
                readonly_enabled(),
            );
            commit_registered_detach(*id, path);
        }
        return Err(error);
    }
    if let Some((id, _)) = &registration {
        commit_registered_detach(*id, path);
    }
    crate::platform::fs::permissions::make_writable(path)?;
    if let Some((_, blob_path)) = registration {
        let _ =
            crate::platform::fs::permissions::set_cache_blob_sealed(&blob_path, readonly_enabled());
    }
    Ok(())
}

#[cfg(test)]
#[path = "hardlink_perf_tests.rs"]
mod perf_tests;
