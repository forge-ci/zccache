//! Retired `v<VERSION>` store accounting for the disk-maintenance pass
//! (issue #1659 follow-up).
//!
//! The retention pass must see the bytes an upgrade left behind in sibling
//! version stores, and must reclaim those before evicting any live entry of
//! the current store.

use std::path::Path;

use crate::core::NormalizedPath;

/// `Some((top_level, current))` when `cache_dir`'s final segment is exactly
/// the running `v<VERSION>`; `None` for a bare (non-versioned) cache root,
/// which has no sibling-version layout to inspect.
pub(super) fn versioned_top_level(cache_dir: &Path) -> Option<(NormalizedPath, String)> {
    let current = crate::core::config::versioned_subdir();
    let is_versioned_root = cache_dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == current.as_str());
    if !is_versioned_root {
        return None;
    }
    let top_level = NormalizedPath::from(cache_dir.parent()?.to_path_buf());
    Some((top_level, current))
}

/// Bytes that removing the retired sibling stores of `top_level` would
/// free: allocated bytes of `nlink == 1` regular files whose blocks are not
/// known to be reflink/snapshot shared (#1673). This is an estimate for the
/// pressure decision, so unknown sharing still counts; see
/// `file_may_free_space_on_removal`. Symlinks/reparse points are never
/// followed; a file whose link count is unknown is treated as shared.
pub(super) fn retired_store_bytes(top_level: &Path, current: &str) -> u64 {
    let Ok(entries) = kernal_api::platform::fs::read_dir_paths(top_level) else {
        return 0;
    };
    let mut total = 0_u64;
    for path in entries {
        if !kernal_api::platform::fs::symlink_metadata(&path)
            .is_ok_and(|metadata| metadata.is_dir())
        {
            continue;
        }
        let Some(name) = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        if !crate::core::config::is_version_dir_name(&name)
            || !crate::core::config::is_older_version_dir(&name, current)
        {
            continue;
        }
        total = total.saturating_add(unshared_tree_bytes(&path));
    }
    total
}

fn unshared_tree_bytes(root: &Path) -> u64 {
    if crate::platform::fs::links::classify(root)
        .is_ok_and(|kind| kind != crate::platform::fs::links::LinkKind::Regular)
    {
        return 0;
    }
    let Ok(entries) = kernal_api::platform::fs::read_dir_paths(root) else {
        return 0;
    };
    let mut total = 0_u64;
    for path in entries {
        let Ok(metadata) = kernal_api::platform::fs::symlink_metadata(&path) else {
            continue;
        };
        if crate::platform::fs::links::classify(&path)
            .is_ok_and(|kind| kind != crate::platform::fs::links::LinkKind::Regular)
        {
            continue;
        }
        if metadata.is_dir() {
            total = total.saturating_add(unshared_tree_bytes(&path));
        } else if metadata.is_file() && crate::core::config::file_may_free_space_on_removal(&path) {
            total = total.saturating_add(crate::platform::fs::volume::allocated_bytes(
                &path, &metadata,
            ));
        }
    }
    total
}
