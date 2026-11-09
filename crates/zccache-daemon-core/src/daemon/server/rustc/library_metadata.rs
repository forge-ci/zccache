//! Bounded, cached inspection of rustc's own metadata; never changes compile argv.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::core::{sync::State, NormalizedPath};
use crate::fscache::MetadataCache;
use crate::hash::ContentHash;

const STATE_WAIT: Duration = Duration::from_secs(1);
const PROBE_WAIT: Duration = Duration::from_secs(30);
const MAX_ENTRIES: usize = 512;

#[derive(Debug)]
pub(super) struct Dependency {
    pub stem: String,
    pub hash: u128,
}

#[derive(Debug)]
pub(super) struct Metadata {
    pub stem: String,
    pub hash: u128,
    pub triple: String,
    pub dependencies: Vec<Dependency>,
}

type ParsedCache = HashMap<(ContentHash, ContentHash), Arc<Metadata>>;
type DirectoryCache = HashMap<(NormalizedPath, ContentHash, String), NormalizedPath>;
static PARSED: OnceLock<State<ParsedCache>> = OnceLock::new();
static DIRECTORIES: OnceLock<State<DirectoryCache>> = OnceLock::new();

#[cfg(test)]
std::thread_local! { static PROBE_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
pub(super) fn probe_count() -> u64 {
    PROBE_COUNT.with(std::cell::Cell::get)
}

fn parsed_cache() -> &'static State<ParsedCache> {
    PARSED.get_or_init(|| State::new(HashMap::new()))
}

fn component(value: &str) -> bool {
    !value.is_empty() && value.len() <= 255 && !value.contains(['/', '\\', ':'])
}

/// `-Zls=root` is an inspection-only rustc facility. Bootstrap is confined to
/// this child: user compilations retain their original stable/nightly policy.
/// Unknown output, including a compiler rejecting the facility, fails closed.
pub(super) fn parse(text: &str) -> Result<Metadata, &'static str> {
    let mut lines = text.lines();
    if lines.next() != Some("Crate info:") {
        return Err("unrecognized rustc library metadata");
    }
    let stem = lines
        .next()
        .and_then(|line| line.strip_prefix("name "))
        .filter(|name| component(name))
        .ok_or("invalid metadata crate name")?;
    let hash = lines
        .next()
        .and_then(|line| line.strip_prefix("hash "))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|hash| u128::from_str_radix(hash, 16).ok())
        .ok_or("invalid metadata crate hash")?;
    let mut triple = None;
    let mut dependencies = Vec::new();
    let mut in_dependencies = false;
    let mut ended_dependencies = false;
    for line in lines {
        if let Some(value) = line.strip_prefix("triple ") {
            triple = Some(value.to_string());
        } else if line == "=External Dependencies=" {
            in_dependencies = true;
        } else if in_dependencies && line.trim().is_empty() {
            ended_dependencies = true;
        } else if in_dependencies && !line.trim().is_empty() {
            let fields: Vec<_> = line.split_whitespace().collect();
            if ended_dependencies
                || fields.len() < 9
                || fields[0].parse::<usize>().ok() != Some(dependencies.len() + 1)
                || !component(fields[1])
                || fields[2] != "hash"
                || fields[4] != "host_hash"
                || fields[5] != "None"
                || fields[6] != "kind"
                || dependencies.len() >= 1024
            {
                return Err("unsupported rustc dependency metadata");
            }
            dependencies.push(Dependency {
                stem: fields[1].to_string(),
                hash: u128::from_str_radix(fields[3], 16).map_err(|_| "invalid dependency hash")?,
            });
        }
    }
    let triple = triple
        .filter(|value| component(value))
        .ok_or("missing metadata target")?;
    if !in_dependencies || !ended_dependencies {
        return Err("missing or incomplete metadata dependency list");
    }
    Ok(Metadata {
        stem: stem.to_string(),
        hash,
        triple,
        dependencies,
    })
}

fn probe(spec: kernal_api::SpawnSpec, compiler: &Path) -> Result<String, &'static str> {
    #[cfg(test)]
    PROBE_COUNT.with(|count| count.set(count.get() + 1));
    let output = crate::core::small_tool::output_for_recorded(
        &compiler.display().to_string(),
        spec,
        PROBE_WAIT,
    )
    .map_err(|_| "rustc library inspection failed")?;
    if output.exit.raw_code() != 0 {
        return Err("rustc library inspection exited unsuccessfully");
    }
    String::from_utf8(output.stdout).map_err(|_| "non-UTF8 rustc library inspection")
}

fn snapshot(
    library: &NormalizedPath,
    digest: ContentHash,
    probe_dir: &Path,
) -> Result<(tempfile::TempDir, std::path::PathBuf), &'static str> {
    let snapshot_dir =
        tempfile::tempdir_in(probe_dir).map_err(|_| "metadata snapshot directory failed")?;
    let snapshot = snapshot_dir
        .path()
        .join(library.file_name().ok_or("invalid library filename")?);
    {
        let mut source =
            std::fs::File::open(library).map_err(|_| "metadata snapshot source failed")?;
        let mut target =
            std::fs::File::create(&snapshot).map_err(|_| "metadata snapshot file failed")?;
        std::io::copy(&mut source, &mut target).map_err(|_| "metadata snapshot copy failed")?;
    }
    if crate::hash::hash_file(&snapshot).ok() != Some(digest) {
        return Err("Rust library changed while snapshotting metadata");
    }
    Ok((snapshot_dir, snapshot))
}

pub(super) fn inspect(
    compiler: &NormalizedPath,
    compiler_hash: ContentHash,
    library: &NormalizedPath,
    files: &MetadataCache,
    probe_dir: &Path,
) -> Result<Arc<Metadata>, &'static str> {
    // The daemon's normal metadata cache owns stat/identity/racy-write checks.
    let digest = files
        .lookup_normalized(library)
        .map_err(|_| "unreadable Rust library")?;
    let key = (compiler_hash, digest);
    if let Some(metadata) =
        parsed_cache().with_blocking_for(STATE_WAIT, |cache| cache.get(&key).map(Arc::clone))
    {
        return Ok(metadata);
    }
    // Inspect an immutable private copy whose digest is the memo key. Merely
    // hashing the original before/after a child would allow an ABA replacement
    // to memoize another file's metadata under the original digest.
    let (_snapshot_dir, snapshot) = snapshot(library, digest, probe_dir)?;
    if library
        .extension()
        .is_some_and(|extension| extension == "rlib")
    {
        super::native_inputs::require_regular_archive(&snapshot)?;
    }
    let spec = super::super::compiler_spawn::rustc_library_metadata_probe_spawn(
        compiler.as_path(),
        &snapshot,
        probe_dir,
    );
    let metadata = Arc::new(parse(&probe(spec, compiler.as_path())?)?);
    if files.lookup_normalized(library).ok() != Some(digest) {
        return Err("Rust library changed during metadata inspection");
    }
    let retired = parsed_cache().with_blocking_for(STATE_WAIT, |cache| {
        let retired = if cache.len() >= MAX_ENTRIES {
            cache
                .keys()
                .next()
                .copied()
                .and_then(|old| cache.remove(&old))
        } else {
            None
        };
        cache.insert(key, Arc::clone(&metadata));
        retired
    });
    drop(retired);
    Ok(metadata)
}

pub(super) fn target_directory(
    compiler: &NormalizedPath,
    compiler_hash: ContentHash,
    triple: &str,
    probe_dir: &Path,
) -> Result<NormalizedPath, &'static str> {
    let cache = DIRECTORIES.get_or_init(|| State::new(HashMap::new()));
    let key = (compiler.clone(), compiler_hash, triple.to_string());
    if let Some(path) = cache.with_blocking_for(STATE_WAIT, |cache| cache.get(&key).cloned()) {
        return Ok(path);
    }
    let spec = super::super::compiler_spawn::rustc_target_library_probe_spawn(
        compiler.as_path(),
        triple,
        probe_dir,
    );
    let output = probe(spec, compiler.as_path())?;
    let path = NormalizedPath::new(output.trim());
    if !path.is_absolute() || !path.is_dir() {
        return Err("unresolved default Rust target libraries");
    }
    cache.with_blocking_for(STATE_WAIT, |cache| {
        if cache.len() >= 64 {
            if let Some(old) = cache.keys().next().cloned() {
                cache.remove(&old);
            }
        }
        cache.insert(key, path.clone());
    });
    Ok(path)
}
