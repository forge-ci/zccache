//! Bounded conservative input inventory for links with many explicit externs.
//!
//! Every possible library candidate participates, including unused ones. The
//! exact metadata resolver remains preferable for small or oversized searches.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use rayon::prelude::*;

use crate::core::{sync::State, NormalizedPath};
use crate::depgraph::ExternCrate;
use crate::fscache::{FileMetadata, MetadataCache};
use crate::hash::ContentHash;

const MAX_FILES: usize = 1024;
const MAX_DIRECTORIES: usize = 64;
const STATE_WAIT: Duration = Duration::from_secs(1);

struct Inventory {
    stamp: FileMetadata,
    observed_at: SystemTime,
    paths: Arc<Vec<NormalizedPath>>,
}

type Directories = HashMap<NormalizedPath, Inventory>;
static DIRECTORIES: OnceLock<State<Directories>> = OnceLock::new();

fn directory(path: &NormalizedPath) -> Result<Option<Arc<Vec<NormalizedPath>>>, &'static str> {
    let observed_at = SystemTime::now();
    let stamp = MetadataCache::stat_file(path).map_err(|_| "unreadable Rust search directory")?;
    let cache = DIRECTORIES.get_or_init(|| State::new(HashMap::new()));
    if let Some(paths) = cache.with_blocking_for(STATE_WAIT, |cache| {
        cache
            .get(path)
            .filter(|entry| {
                // Do not memoize membership observed inside the filesystem's racy
                // timestamp tick, including platforms without ctime/inode identity.
                entry.stamp.identity.change_ns != 0
                    && entry.stamp.identity.file_id != 0
                    && entry.stamp.same_stamp(&stamp)
                    && entry
                        .observed_at
                        .checked_sub(Duration::from_secs(1))
                        .is_some_and(|floor| entry.stamp.mtime < floor)
            })
            .map(|entry| Arc::clone(&entry.paths))
    }) {
        return Ok(Some(paths));
    }
    let mut paths = Vec::new();
    for (index, entry) in std::fs::read_dir(path)
        .map_err(|_| "unreadable Rust search directory")?
        .enumerate()
    {
        if index >= MAX_FILES {
            return Ok(None);
        }
        let candidate = entry.map_err(|_| "unreadable Rust search entry")?.path();
        if !candidate.extension().is_some_and(|extension| {
            matches!(
                extension.to_str(),
                Some("rmeta" | "rlib" | "so" | "dylib" | "dll" | "lib" | "a")
            )
        }) {
            continue;
        }
        // Retain names even when a symlink currently resolves to a directory:
        // its target can become a file without changing this directory's stamp.
        paths.push(candidate.into());
    }
    let after = MetadataCache::stat_file(path).map_err(|_| "Rust search directory disappeared")?;
    if !stamp.same_stamp(&after) {
        return Err("Rust search directory changed during inventory");
    }
    paths.sort();
    let paths = Arc::new(paths);
    let entry = Inventory {
        stamp,
        observed_at,
        paths: Arc::clone(&paths),
    };
    let retired = cache.with_blocking_for(STATE_WAIT, |cache| {
        let evicted = if cache.len() >= MAX_DIRECTORIES {
            cache
                .keys()
                .next()
                .cloned()
                .and_then(|key| cache.remove(&key))
        } else {
            None
        };
        (evicted, cache.insert(path.clone(), entry))
    });
    drop(retired);
    Ok(Some(paths))
}

pub(super) fn inputs(
    dirs: &[NormalizedPath],
    compiler: &NormalizedPath,
    compiler_hash: ContentHash,
    target: Option<&str>,
    files: &MetadataCache,
    probe_dir: &std::path::Path,
) -> Result<Option<Vec<ExternCrate>>, &'static str> {
    let sysroot =
        super::library_metadata::target_directory(compiler, compiler_hash, target, probe_dir)?;
    let mut search = dirs.to_vec();
    if !search.contains(&sysroot) {
        search.push(sysroot);
    }
    let mut inventory = Vec::new();
    for (index, dir) in search.iter().enumerate() {
        let Some(paths) = directory(dir)? else {
            return Ok(None);
        };
        if inventory.len() + paths.len() > MAX_FILES {
            return Ok(None);
        }
        inventory.extend(paths.iter().map(|path| (index, path.clone())));
    }
    let inputs = inventory
        .into_par_iter()
        .map(|(index, path)| {
            if !std::fs::metadata(&path)
                .map_err(|_| "unreadable Rust candidate")?
                .is_file()
            {
                return Ok(None);
            }
            // Only membership is memoized. Each candidate still passes the normal
            // stat/identity/racy-write content verifier on every request.
            let hash = files
                .lookup_normalized(&path)
                .map_err(|_| "unreadable Rust input")?;
            if path
                .extension()
                .is_some_and(|extension| extension == "rlib")
            {
                super::library_metadata::regular_archive(&path, hash, probe_dir)?;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("invalid Rust library filename")?;
            Ok(Some(ExternCrate {
                name: format!("__zccache_rust_inventory__:{index}:{name}:{hash}"),
                path,
            }))
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    Ok(Some(inputs.into_iter().flatten().collect()))
}

#[cfg(test)]
#[path = "library_inventory_tests.rs"]
mod tests;
