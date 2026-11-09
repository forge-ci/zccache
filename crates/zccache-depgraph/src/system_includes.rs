//! System include path discovery from compiler output.
//!
//! Parses the output of `<compiler> -v -E -x c++ /dev/null 2>&1` to extract
//! the compiler's default system include search paths. These paths are used
//! to resolve `#include <...>` directives that don't match any explicit
//! `-I`/`-isystem` paths.
//!
//! The discovery command differs by platform:
//! - Linux/macOS: `<compiler> -v -E -x c++ /dev/null 2>&1`
//! - Windows: `<compiler> -v -E -x c++ NUL 2>&1`
//!
//! The actual command execution is left to the caller (daemon). This module
//! only handles parsing the output and caching results.
//!
//! ## On-disk persistence (issue #541)
//!
//! Spawning the compiler with `-v -E` to scrape `#include <...>` lines costs
//! ~30-50 ms (Linux, more on Windows) — paid on every first-after-restart
//! C/C++ compile of a daemon. The cache persists `(compiler_path, mtime,
//! size) -> include_paths` to disk alongside `metadata.bin` so subsequent
//! daemon runs against the same toolchain skip the spawn entirely. Stat
//! verification on load catches in-place compiler upgrades (apt upgrade,
//! Homebrew `brew upgrade clang`, etc.) — a mismatch silently rediscovers,
//! so a stale snapshot cannot poison the cache.

use std::collections::HashMap;
use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use zccache_core::NormalizedPath;

/// Parse compiler `-v -E` output to extract system include paths.
///
/// Looks for the section between `#include <...> search starts here:`
/// and `End of search list.` in the compiler's stderr output.
///
/// Each line in that section is trimmed and treated as a directory path.
/// Lines starting with ` (framework directory)` are included but the
/// suffix is stripped.
#[must_use]
pub fn parse_system_include_output(output: &str) -> Vec<NormalizedPath> {
    parse_system_include_output_checked(output).unwrap_or_default()
}

/// A complete search-list section proves the roots, including an explicitly empty list.
/// Missing either marker is degraded output, never a reusable empty result.
#[must_use]
pub fn parse_system_include_output_checked(output: &str) -> Option<Vec<NormalizedPath>> {
    let mut in_section = false;
    let mut paths = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();

        if trimmed == "#include <...> search starts here:" {
            in_section = true;
            continue;
        }

        if in_section && trimmed == "End of search list." {
            return Some(paths);
        }

        if in_section && !trimmed.is_empty() {
            // Some compilers annotate framework dirs: "/path (framework directory)"
            let path_str = if let Some(stripped) = trimmed.strip_suffix(" (framework directory)") {
                stripped
            } else {
                trimmed
            };

            if !path_str.is_empty() {
                paths.push(path_str.into());
            }
        }
    }

    None
}

/// Name of the MSVC environment variable that lists the system include roots.
///
/// `vcvarsall.bat` (and every `vcvars<arch>.bat` wrapper) exports `INCLUDE` as
/// a `;`-separated list of directories. `cl.exe` resolves every
/// `#include <...>` against that list -- it has no compiled-in default search
/// path and no `-v -E` style discovery mode.
pub const MSVC_INCLUDE_ENV: &str = "INCLUDE";

/// Split an MSVC `INCLUDE` value into system include roots.
///
/// The value is `;`-separated. Segments are trimmed and empty segments are
/// dropped -- a trailing `;` is idiomatic in `vcvars`-generated values and must
/// not produce a bogus empty root.
#[must_use]
pub fn parse_msvc_include_env(value: &str) -> Vec<NormalizedPath> {
    value
        .split(';')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(NormalizedPath::new)
        .collect()
}

/// Extract the MSVC system include roots from a client environment block.
///
/// Windows environment variable names are case-insensitive, and the block is
/// forwarded verbatim from the client shell, so the lookup must be too. The
/// last matching entry wins, mirroring how a process environment collapses
/// duplicate names.
#[must_use]
pub fn msvc_system_includes_from_env(env: &[(String, String)]) -> Vec<NormalizedPath> {
    env.iter()
        .rev()
        .find(|(name, _)| name.eq_ignore_ascii_case(MSVC_INCLUDE_ENV))
        .map(|(_, value)| parse_msvc_include_env(value))
        .unwrap_or_default()
}

/// Build the compiler discovery command arguments.
///
/// Returns the arguments to pass to the compiler to discover system include
/// paths. The caller should execute the compiler with these args and capture
/// stderr.
#[must_use]
pub fn discovery_args() -> Vec<&'static str> {
    if kernal_api::platform::host::target_is_windows() {
        vec!["-v", "-E", "-x", "c++", "NUL"]
    } else {
        vec!["-v", "-E", "-x", "c++", "/dev/null"]
    }
}

/// Build fallback discovery arguments for a C-only compiler installation.
///
/// Some minimal Linux images install `cc`/GCC without the C++ frontend.  The
/// normal C++ probe then fails before printing an include search list even
/// though the compiler can cache C translation units perfectly well.
#[must_use]
pub fn discovery_args_c() -> Vec<&'static str> {
    if kernal_api::platform::host::target_is_windows() {
        vec!["-v", "-E", "-x", "c", "NUL"]
    } else {
        vec!["-v", "-E", "-x", "c", "/dev/null"]
    }
}

/// Build the **fast** clang-family discovery command arguments.
///
/// Issue #541 option (B): `clang -###` makes clang print the `-cc1`
/// command-line it WOULD execute, then exit without spawning cc1. The
/// printed line includes `-internal-isystem` / `-internal-externc-isystem`
/// flags pointing at every system include path the driver would have
/// passed to the real preprocessor. Wall-clock: ~3-5 ms vs the ~30-50 ms
/// of the full `-v -E` discovery.
///
/// Only safe for clang-family compilers (clang, clang++, em++,
/// clang-cl). Gcc accepts `-###` but emits a different format that
/// doesn't include the per-path `-internal-isystem` flags this parser
/// expects. Use [`discovery_args`] (the slow path) for gcc/MSVC.
#[must_use]
pub fn discovery_args_fast() -> Vec<&'static str> {
    if kernal_api::platform::host::target_is_windows() {
        vec!["-###", "-E", "-x", "c++", "NUL"]
    } else {
        vec!["-###", "-E", "-x", "c++", "/dev/null"]
    }
}

/// Parse the `-cc1` line from clang's `-###` output and extract every
/// `-iwithsysroot`, `-internal-isystem`, and `-internal-externc-isystem` path.
///
/// The driver prints exactly one line per invocation that looks like:
///
/// ```text
///  "/usr/lib/llvm-18/bin/clang-18" "-cc1" "-triple" "x86_64-pc-linux-gnu" "-E" \
///    "-internal-isystem" "/usr/lib/llvm-18/lib/clang/18/include" \
///    "-internal-isystem" "/usr/local/include" \
///    "-internal-externc-isystem" "/usr/include/x86_64-linux-gnu" \
///    "-internal-externc-isystem" "/usr/include" ...
/// ```
///
/// Tokens are double-quoted; spaces inside paths are preserved by the
/// quoting. The tokenizer splits on `"` and reads odd-indexed segments
/// (the actual argument values, alternating with single-space gaps).
///
/// Returns the include paths in driver-emitted order (which matches
/// clang's actual `#include <...>` search order — the first `-cc1`
/// invocation's order is canonical).
#[must_use]
pub fn parse_cc1_system_include_output(output: &str) -> Vec<NormalizedPath> {
    let mut paths = Vec::new();
    for line in output.lines() {
        // Cheap shape probe — the cc1 line contains `-cc1` either as a
        // quoted token (clang on macOS / Windows) or as a bare argument
        // (clang on most Linux builds — issue #544). Both forms have
        // `-cc1` as a standalone whitespace-or-quote-bounded token, so
        // a substring match catches both.
        if !line.contains("-cc1") {
            continue;
        }
        let tokens = tokenize_command_line(line);
        // Reject lines that don't actually have `-cc1` as a TOKEN
        // (e.g. a comment that happens to contain the string "-cc1").
        if !tokens.iter().any(|t| t == "-cc1") {
            continue;
        }
        let sysroot = tokens
            .windows(2)
            .find_map(|pair| (pair[0] == "-isysroot").then_some(pair[1].as_str()));
        let mut with_sysroot = Vec::new();
        let mut internal = Vec::new();
        let mut iter = tokens.iter().peekable();
        while let Some(token) = iter.next() {
            if token == "-internal-isystem" || token == "-internal-externc-isystem" {
                if let Some(path) = iter.next() {
                    if !path.is_empty() {
                        internal.push(NormalizedPath::new(path));
                    }
                }
            } else if let Some(joined_path) = token.strip_prefix("-iwithsysroot") {
                let path = if joined_path.is_empty() {
                    iter.next().map(String::as_str)
                } else {
                    Some(joined_path)
                };
                if let Some(path) = path.filter(|path| !path.is_empty()) {
                    let resolved = sysroot.map_or_else(
                        || NormalizedPath::new(path),
                        |root| {
                            NormalizedPath::new(
                                Path::new(root).join(path.trim_start_matches(['/', '\\'])),
                            )
                        },
                    );
                    with_sysroot.push(resolved);
                }
            }
        }
        // Clang appends these flags to the cc1 argv after its internal
        // directories, but searches them first (as shown by `-v -E`).
        paths.extend(with_sysroot);
        paths.extend(internal);
        // Only the first cc1 line — multi-arch / multi-tu drivers may
        // emit several but the include search order is identical.
        if !paths.is_empty() {
            break;
        }
    }
    paths
}

/// Tokenize a clang `-###` command line, handling both `"quoted"` and
/// bare whitespace-separated arguments.
///
/// Some clang builds quote every arg (`"/path/to/clang" "-cc1" "-E" …`),
/// others quote only the binary path with bare args after
/// (`"/path/to/clang" -cc1 -E …`). The previous parser split strictly
/// on `"` and missed the bare-arg form — issue #544.
///
/// Empty tokens are dropped. Quotes inside a quoted segment preserve
/// the space-containing path as one token; backslash escapes are NOT
/// supported (clang doesn't emit them in `-###` output).
fn tokenize_command_line(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    for c in line.chars() {
        if c == '"' {
            in_quote = !in_quote;
        } else if c.is_whitespace() && !in_quote {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// On-disk format version for the persisted `SystemIncludeCache` snapshot.
///
/// Bump on any layout change so the loader rejects older / newer snapshots
/// instead of mis-decoding them.
pub const FORMAT_VERSION: u32 = 2;

/// One cache entry — the discovered include paths plus the (mtime, size)
/// fingerprint of the compiler binary that produced them. Verifying stat
/// on lookup catches in-place compiler upgrades that would otherwise
/// serve stale include paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemIncludeEntry {
    pub mtime: SystemTime,
    pub size: u64,
    pub paths: Vec<NormalizedPath>,
    /// Only an explicit complete search list may confirm zero roots (#1879).
    pub known_empty: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedSystemIncludes {
    version: u32,
    entries: Vec<(NormalizedPath, SystemIncludeEntry)>,
}

/// Cache of discovered system include paths, keyed by compiler path.
///
/// Avoids re-running the compiler discovery command for the same compiler
/// across sessions. Entries store a (mtime, size) fingerprint of the
/// compiler binary; stat-verify on lookup rediscovers if the binary changed.
#[derive(Debug, Default, Clone)]
pub struct SystemIncludeCache {
    cache: HashMap<NormalizedPath, SystemIncludeEntry>,
    /// Set by every mutating method; cleared by [`Self::take_dirty`]. Lets a
    /// shutdown save skip re-serializing and re-fsyncing the snapshot when
    /// nothing changed since the last save (zccache#1775). `merge_from`
    /// (background disk load) deliberately does not set this — the merged
    /// content already matches what's on disk. Plain `bool` (not atomic):
    /// the cache is only ever accessed through the daemon's
    /// `Mutex<SystemIncludeCache>`, never shared unsynchronized.
    dirty: bool,
}

impl SystemIncludeCache {
    /// Create an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the cache has been mutated since the last [`Self::take_dirty`].
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Clear the dirty flag, returning whether it was set.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Look up cached system include paths for a compiler, verifying stat.
    ///
    /// Returns `None` when either the cache has no entry, its path list is
    /// unconfirmed and empty, OR the compiler binary's (mtime, size) no longer
    /// matches the entry's fingerprint. Confirmed empty roots are reusable.
    /// Stat errors (e.g., compiler removed) also return `None` so the caller
    /// falls through to rediscovery.
    #[must_use]
    pub fn get(&self, compiler: &Path) -> Option<&[NormalizedPath]> {
        let key = NormalizedPath::new(compiler);
        let entry = self.cache.get(&key)?;
        let metadata = std::fs::metadata(compiler).ok()?;
        let mtime = metadata.modified().ok()?;
        let size = metadata.len();
        if (entry.known_empty || !entry.paths.is_empty())
            && entry.mtime == mtime
            && entry.size == size
        {
            Some(entry.paths.as_slice())
        } else {
            None
        }
    }

    /// Store discovered system include paths for a compiler.
    ///
    /// Captures the compiler binary's (mtime, size) at insert time so a
    /// subsequent `get` can stat-verify. Empty results are deliberately not
    /// cached: an apparently successful probe with no paths is degraded and
    /// must be retried on the next compile. If the stat fails (compiler
    /// removed mid-insert), the entry is silently dropped — better to
    /// re-discover than to cache without a valid fingerprint.
    pub fn insert(&mut self, compiler: NormalizedPath, paths: Vec<NormalizedPath>) {
        if paths.is_empty() {
            return;
        }
        self.insert_entry(compiler, paths, false);
    }

    /// Store a confirmed empty search list; callers must have parsed both markers.
    pub fn insert_known_empty(&mut self, compiler: NormalizedPath) {
        self.insert_entry(compiler, Vec::new(), true);
    }

    fn insert_entry(
        &mut self,
        compiler: NormalizedPath,
        paths: Vec<NormalizedPath>,
        known_empty: bool,
    ) {
        let Ok(metadata) = std::fs::metadata(compiler.as_path()) else {
            return;
        };
        let Ok(mtime) = metadata.modified() else {
            return;
        };
        let size = metadata.len();
        self.cache.insert(
            compiler,
            SystemIncludeEntry {
                mtime,
                size,
                paths,
                known_empty,
            },
        );
        self.dirty = true;
    }

    /// Get cached paths or discover them using the provided closure.
    ///
    /// Performs the same stat-verify as `get`. On a verified hit, the
    /// discovery closure is not invoked. On a miss (no entry OR stat
    /// mismatch), the closure runs and its result is cached with a
    /// fresh fingerprint.
    pub fn get_or_discover<F>(&mut self, compiler: &Path, discover: F) -> &[NormalizedPath]
    where
        F: FnOnce(&Path) -> Vec<NormalizedPath>,
    {
        let compiler_key = NormalizedPath::new(compiler);
        let stat_match = self.cache.get(&compiler_key).is_some_and(|entry| {
            (entry.known_empty || !entry.paths.is_empty())
                && std::fs::metadata(compiler)
                    .ok()
                    .and_then(|m| m.modified().ok().map(|mt| (mt, m.len())))
                    .is_some_and(|(mt, size)| mt == entry.mtime && size == entry.size)
        });
        if !stat_match {
            let paths = discover(compiler);
            self.insert(compiler_key.clone(), paths);
        }
        self.cache
            .get(&compiler_key)
            .map(|e| e.paths.as_slice())
            .unwrap_or(&[])
    }

    /// Remove all cached entries.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.dirty = true;
    }

    /// Drain entries from a freshly loaded `SystemIncludeCache` into
    /// `self` using `HashMap::extend`. Empty entries from snapshots written
    /// by older releases are discarded because they are not valid reusable
    /// discovery results.
    ///
    /// Issue #784 phase 2c: lets a background `spawn_blocking` task
    /// load the on-disk snapshot AFTER the daemon has written its
    /// readiness lockfile, then populate the live cache without
    /// holding up bind. Readers during the merge window either see no
    /// entry (cold-path miss — the next `get_or_discover` re-runs the
    /// compiler probe) or a loaded entry (stat-verify at the call site
    /// rejects a stale `(mtime, size)` before trusting the cached
    /// paths, so a partially-loaded snapshot cannot poison results).
    pub fn merge_from(&mut self, other: Self) {
        self.cache.extend(
            other
                .cache
                .into_iter()
                .filter(|(_, entry)| entry.known_empty || !entry.paths.is_empty()),
        );
    }

    /// Number of cached entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Check if the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Persist the cache to `path` as a versioned bincode snapshot.
    ///
    /// Atomic on success: writes to `<path>.tmp-<pid>`, then renames over
    /// `path`. Empty snapshots short-circuit without touching disk. Stale
    /// entries on disk are harmless: `get` re-stats every key before
    /// trusting the cached entry, so a mismatch silently downgrades to a
    /// re-discovery on the daemon side.
    ///
    /// # Errors
    ///
    /// Returns I/O errors from `create_dir_all`, `write`, `rename`, or
    /// bincode serialization.
    pub fn save_to_disk(&self, path: &Path) -> std::io::Result<()> {
        let entries: Vec<(NormalizedPath, SystemIncludeEntry)> = self
            .cache
            .iter()
            .filter(|(_, entry)| entry.known_empty || !entry.paths.is_empty())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        if entries.is_empty() {
            tracing::debug!(
                path = %path.display(),
                "system include cache flush: 0 entries, skipping write"
            );
            return Ok(());
        }

        let entry_count = entries.len();
        let snapshot = PersistedSystemIncludes {
            version: FORMAT_VERSION,
            entries,
        };
        let bytes = bincode::serialize(&snapshot)
            .map_err(|e| std::io::Error::other(format!("bincode serialize: {e}")))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "system_includes.bin".into());
        let tmp = path.with_file_name(format!(".{name}.tmp-{}", std::process::id()));

        let result = write_atomic_durable(&tmp, path, &bytes);
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        if result.is_ok() {
            tracing::info!(
                path = %path.display(),
                entries = entry_count,
                bytes = bytes.len(),
                "system include cache flushed to disk"
            );
        }
        result
    }

    /// Load a previously persisted snapshot from `path`.
    ///
    /// Returns an empty cache when the file is absent (first run). Any
    /// other I/O error, bincode decode failure, or version mismatch is
    /// surfaced as `Err`; the daemon caller is expected to log and start
    /// empty. Stat-verification at `get` re-checks every loaded entry,
    /// so a stale on-disk snapshot cannot produce incorrect includes.
    ///
    /// # Errors
    ///
    /// Any I/O error other than `NotFound`, any bincode decode failure,
    /// or any version mismatch.
    pub fn load_from_disk(path: &Path) -> std::io::Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    path = %path.display(),
                    "system include cache file not found, starting empty"
                );
                return Ok(Self::new());
            }
            Err(e) => return Err(e),
        };

        let snapshot: PersistedSystemIncludes = bincode::deserialize(&bytes)
            .map_err(|e| std::io::Error::other(format!("bincode deserialize: {e}")))?;
        if snapshot.version != FORMAT_VERSION {
            return Err(std::io::Error::other(format!(
                "system include cache version mismatch: file={}, expected={}",
                snapshot.version, FORMAT_VERSION
            )));
        }

        let mut cache: HashMap<NormalizedPath, SystemIncludeEntry> =
            HashMap::with_capacity(snapshot.entries.len());
        let entry_count = snapshot.entries.len();
        for (key, value) in snapshot.entries {
            if value.known_empty || !value.paths.is_empty() {
                cache.insert(key, value);
            }
        }
        tracing::info!(
            path = %path.display(),
            entries = entry_count,
            "system include cache restored from disk"
        );
        Ok(Self {
            cache,
            dirty: false,
        })
    }
}

fn write_atomic_durable(tmp: &Path, target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    {
        let mut f = std::fs::File::create(tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(tmp, target)?;
    if let Some(parent) = target.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
