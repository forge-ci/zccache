//! Per-crate fingerprint revalidation for `zccache`-managed target snapshots.
//!
//! # Why this exists
//!
//! `actions/checkout` writes every source file with mtime ≈ "now", and
//! `actions/cache` brings a `target/` snapshot back with mtimes from when
//! that snapshot was originally tarred (always in the past). Cargo's
//! freshness check (`source.mtime > dep_info.mtime → rebuild`) then sees
//! every source as newer than every fingerprint anchor and cold-rebuilds
//! the whole workspace. The original `action.yml` workaround — touching
//! every file in `target/` to "now + 1 minute" — defeated the check too
//! aggressively: cargo trusted the cache even for crates whose source had
//! actually changed, silently linking the previous commit's compiled
//! binary against the new source tree (see PR #279 for the bug).
//!
//! The optimal fix is to use a content hash instead of mtime to decide
//! freshness. Stable cargo doesn't support that (`-Z checksum-freshness`
//! is nightly-only), so we replicate the same idea per crate in a sidecar:
//!
//! * **Save side** (`record`): before tarring, walk every cargo fingerprint
//!   dir under `target/<profile>/.fingerprint/`. For each compilation unit
//!   tracked by that fingerprint, find its source-input list and
//!   blake3-hash every workspace-local source. Emit a JSON sidecar at
//!   `<target>/<MANIFEST_FILENAME>` listing per-crate
//!   `(fingerprint_dir, stamp_targets, source_path → blake3)` entries.
//! * **Restore side** (`validate`): rehash every recorded source in
//!   parallel. For each crate where *every* tracked source still matches
//!   its recorded hash, future-stamp that crate's `stamp_targets` so
//!   cargo trusts the cache. Crates with any mismatch are left untouched
//!   so cargo's normal stale check correctly rebuilds them.
//!
//! ## Fingerprint kinds handled
//!
//! 1. **Library / bin / test compilation** (cargo's `CheckDepInfo`
//!    LocalFingerprint). The dep file lives at
//!    `<fp_dir>/dep-{lib,bin,test}-<...>`; the matching `.d` Makefile is
//!    at `target/<profile>/deps/<crate>-<hash>.d`, where `<crate>` is the
//!    target name with `-` mapped to `_` (see [`rustc_dep_info_name`]).
//! 2. **Build script compilation** (also `CheckDepInfo`). The dep file
//!    is `<fp_dir>/dep-build-script-build-script-build`; the `.d` file
//!    lives at `target/<profile>/build/<fp_dir>/build_script_build-<hash>.d`.
//! 3. **Build script execution** (`RerunIfChanged`). No `.d` file; the
//!    json fingerprint lists `paths` relative to the crate's manifest
//!    directory and an `output` file in `target/<profile>/build/<fp_dir>/`
//!    that cargo treats as the freshness anchor.
//!
//! Together these cover every freshness signal cargo uses for workspace
//! crates, so the blanket post-restore `find ... -touch '+1 min'` step in
//! `action.yml` is no longer necessary.

use std::collections::{BTreeMap, HashMap};
use std::fs::FileTimes;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Name of the sidecar manifest, written under `<target>/`. Lives inside
/// `target/` so it rides the existing snapshot tar without needing
/// special handling in `prepare-target-snapshot.sh`.
pub const MANIFEST_FILENAME: &str = ".zccache-fp-manifest.json";

/// Manifest schema version. Bump if you change the layout. `validate`
/// refuses to operate on unknown versions and exits cleanly so a snapshot
/// saved by an older daemon doesn't poison a new run.
///
/// v2: generalised `dep_info_files` (list of names inside the fingerprint
/// dir) into `stamp_targets` (list of paths relative to target_dir) so we
/// can address both the in-fingerprint anchors used by `CheckDepInfo` and
/// the `out`-style anchor files used by `RerunIfChanged`.
const MANIFEST_VERSION: u32 = 2;

#[derive(Serialize, Deserialize, Debug)]
pub struct Manifest {
    pub version: u32,
    /// Informational; `epoch:<unix-seconds>` at record time.
    pub saved_at: String,
    /// Workspace root recorded at save time (forward-slash path). Stored
    /// for diagnostics; restore re-anchors against the runtime workspace.
    pub workspace_root: String,
    pub crates: Vec<CrateEntry>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CrateEntry {
    /// Directory name under `target/<profile>/.fingerprint/`, e.g.
    /// `zccache-daemon-0020565b109aa8e3`. Diagnostic only.
    pub fingerprint_dir: String,
    /// Files to future-stamp if *every* recorded source still matches its
    /// hash. Paths are stored relative to `target_dir`. Lib/bin/test
    /// entries point at the in-`.fingerprint/` anchor; build-script-run
    /// entries point at the output file referenced by `RerunIfChanged`.
    pub stamp_targets: Vec<String>,
    /// Workspace-relative source path → blake3 hex digest.
    pub sources: BTreeMap<String, String>,
}

/// Walk `target/<profile>/.fingerprint/` and emit a manifest covering
/// every CheckDepInfo / RerunIfChanged fingerprint with workspace-local
/// source inputs. Hashing runs in parallel.
pub fn record(
    target_dir: &Path,
    workspace_root: &Path,
    manifest_path: &Path,
    profile: &str,
) -> io::Result<RecordStats> {
    let fp_root = target_dir.join(profile).join(".fingerprint");
    if !fp_root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("fingerprint dir not found: {}", fp_root.display()),
        ));
    }
    let deps_root = target_dir.join(profile).join("deps");
    let build_root = target_dir.join(profile).join("build");
    let workspace_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());

    // For RerunIfChanged we need to resolve `paths` (relative to the
    // crate's manifest directory) against the workspace. Discover the
    // package-name → manifest-dir mapping once up front.
    let manifest_dirs = discover_workspace_crates(&workspace_root)?;

    // Phase 1: scan each `<fp_dir>/` for processable units.
    let fp_dirs: Vec<PathBuf> = read_dir_paths(&fp_root)?
        .into_iter()
        .filter(|p| p.is_dir())
        .collect();

    // Phase 2: process all units in parallel. A single fingerprint dir
    // can produce *multiple* manifest entries — e.g. one for the build
    // script's compilation (`dep-build-script-build-script-build`) and
    // one for its execution (`run-build-script-build-script-build.json`).
    let scans: Vec<FingerprintScan> = fp_dirs
        .par_iter()
        .map(|fp_dir| {
            process_fingerprint_dir(
                fp_dir,
                &fp_root,
                &deps_root,
                &build_root,
                &workspace_root,
                &manifest_dirs,
                profile,
            )
        })
        .collect::<io::Result<_>>()?;
    let mut entries = Vec::new();
    let mut unresolved_dep_info = Vec::new();
    for scan in scans {
        entries.extend(scan.entries);
        unresolved_dep_info.extend(scan.unresolved_dep_info);
    }
    unresolved_dep_info.sort();

    let stats = RecordStats {
        crates_recorded: entries.len(),
        sources_hashed: entries.iter().map(|c| c.sources.len()).sum(),
        unresolved_dep_info,
    };

    let manifest = Manifest {
        version: MANIFEST_VERSION,
        saved_at: chrono_like_now(),
        workspace_root: path_to_unix(&workspace_root),
        crates: entries,
    };

    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let serialized = serde_json::to_string(&manifest)
        .map_err(|e| io::Error::other(format!("serialize manifest: {e}")))?;
    std::fs::write(manifest_path, serialized)?;

    Ok(stats)
}

#[derive(Debug, Default)]
pub struct RecordStats {
    pub crates_recorded: usize,
    pub sources_hashed: usize,
    /// Fingerprint dep files whose rustc `.d` file could not be located, as
    /// `<fingerprint_dir>/<dep_file>`. Those units have no source coverage.
    pub unresolved_dep_info: Vec<String>,
}

#[derive(Default)]
struct FingerprintScan {
    entries: Vec<CrateEntry>,
    unresolved_dep_info: Vec<String>,
}

/// Map a fingerprint dep file to the name of rustc's `.d` file in `deps/`.
///
/// Cargo names the fingerprint dir after the package and the dep file
/// `dep-[test-]<kind>-<target>`, while rustc names the `.d` file after the
/// crate: `<target with - mapped to _>-<hash>`.
fn rustc_dep_info_name(dep_file: &str, dir_name: &str) -> Option<String> {
    let hash = &dir_name[strip_hash_suffix(dir_name)?.len() + 1..];
    let unit = dep_file.strip_prefix("dep-")?;
    let unit = unit.strip_prefix("test-").unwrap_or(unit);
    let target = ["lib-", "bin-", "integration-test-", "example-", "bench-"]
        .iter()
        .find_map(|kind| unit.strip_prefix(kind))?;
    Some(format!("{}-{hash}.d", target.replace('-', "_")))
}

/// Inspect a single `<fp_root>/<fp_dir>` for manifest entries and unresolved units.
fn process_fingerprint_dir(
    fp_dir: &Path,
    fp_root: &Path,
    deps_root: &Path,
    build_root: &Path,
    workspace_root: &Path,
    manifest_dirs: &HashMap<String, PathBuf>,
    profile: &str,
) -> io::Result<FingerprintScan> {
    let Some(dir_name) = fp_dir.file_name().and_then(|n| n.to_str()) else {
        return Ok(FingerprintScan::default());
    };
    let mut entries = Vec::new();
    let mut unresolved_dep_info = Vec::new();

    // List of (dep_file_basename, .d_path) for CheckDepInfo-style units.
    let mut depinfo_units: Vec<(String, PathBuf)> = Vec::new();
    let mut has_run_build_script = false;

    for path in read_dir_paths(fp_dir)? {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if name == "run-build-script-build-script-build.json" {
            has_run_build_script = true;
        } else if name == "dep-build-script-build-script-build" {
            // Build-script compilation. .d file lives in build/<dir>/.
            if let Some(d_file) = first_buildscript_d_file(build_root, dir_name)? {
                depinfo_units.push((name, d_file));
            } else {
                unresolved_dep_info.push(format!("{dir_name}/{name}"));
            }
        } else if name.starts_with("dep-") && !name.starts_with("dep-doc-") {
            // Lib/bin/test compilation. .d file lives in deps/. Rustdoc units
            // have no rustc `.d` file and are not source-tracked here.
            let d_file = rustc_dep_info_name(&name, dir_name)
                .map(|d_name| deps_root.join(d_name))
                .filter(|d_file| d_file.is_file());
            match d_file {
                Some(d_file) => depinfo_units.push((name, d_file)),
                None => unresolved_dep_info.push(format!("{dir_name}/{name}")),
            }
        }
    }

    // Group CheckDepInfo units of the same fingerprint dir into one
    // entry, since they share the same source-input list (they're all
    // for the same compilation invocation).
    if !depinfo_units.is_empty() {
        // All depinfo units in the same fp_dir reference the same .d file
        // by definition. Use the first.
        let d_file = depinfo_units[0].1.clone();
        if let Some(sources) = hash_d_file_sources(&d_file, workspace_root)? {
            let stamp_targets: Vec<String> = depinfo_units
                .iter()
                .map(|(name, _)| {
                    let abs = fp_root.join(dir_name).join(name);
                    relativize_to_target(&abs, target_dir_from_fp_root(fp_root, profile))
                })
                .collect();
            entries.push(CrateEntry {
                fingerprint_dir: dir_name.to_string(),
                stamp_targets,
                sources,
            });
        }
    }

    if has_run_build_script {
        if let Some(entry) =
            process_run_build_script(fp_dir, dir_name, workspace_root, manifest_dirs)?
        {
            entries.push(entry);
        }
    }

    Ok(FingerprintScan {
        entries,
        unresolved_dep_info,
    })
}

/// Names the path in an I/O error that would otherwise reach the user bare.
fn with_path(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

/// Every entry of `dir`, failing on anything short of the complete listing.
fn read_dir_paths(dir: &Path) -> io::Result<Vec<PathBuf>> {
    std::fs::read_dir(dir)
        .and_then(|entries| {
            entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect()
        })
        .map_err(|error| with_path(dir, error))
}

fn target_dir_from_fp_root<'a>(fp_root: &'a Path, _profile: &str) -> &'a Path {
    // fp_root = <target>/<profile>/.fingerprint
    // target  = parent of <profile>
    fp_root.parent().and_then(|p| p.parent()).unwrap_or(fp_root)
}

/// Resolve an absolute path under target/ to a target-relative
/// forward-slash string.
fn relativize_to_target(abs: &Path, target_dir: &Path) -> String {
    let abs = abs.canonicalize().unwrap_or_else(|_| abs.to_path_buf());
    let target = target_dir
        .canonicalize()
        .unwrap_or_else(|_| target_dir.to_path_buf());
    match abs.strip_prefix(&target) {
        Ok(rel) => path_to_unix(rel),
        Err(_) => path_to_unix(abs.as_path()),
    }
}

/// Pick the first build-script `.d` file inside
/// `<build_root>/<dir_name>/`. Cargo emits `build_script_<scriptname>-<hash>.d`
/// — we don't care about the exact name, only that it lists the script's
/// source inputs.
fn first_buildscript_d_file(build_root: &Path, dir_name: &str) -> io::Result<Option<PathBuf>> {
    let listing =
        zccache_core::scan_entry::entry_if_present(read_dir_paths(&build_root.join(dir_name)))?;
    Ok(listing.and_then(|paths| {
        paths
            .into_iter()
            .find(|path| path.extension().and_then(|e| e.to_str()) == Some("d"))
    }))
}

/// Parse a `.d` Makefile, hash each tracked workspace-local source, and
/// return the (workspace-relative path -> blake3 hex) map. Returns `None`
/// if no workspace-local source was found (so the crate is omitted from
/// the manifest -- cargo's normal mtime check applies).
///
/// Relative paths are resolved against `workspace_root`, the directory cargo
/// runs rustc from, never the process cwd. A listed source that no longer
/// exists is skipped, since cargo rebuilds that unit anyway; a source that
/// exists but cannot be read fails the whole record, naming the path.
fn hash_d_file_sources(
    d_file: &Path,
    workspace_root: &Path,
) -> io::Result<Option<BTreeMap<String, String>>> {
    let content = std::fs::read_to_string(d_file).map_err(|e| with_path(d_file, e))?;
    let sources = parse_d_file(&content);
    let hashed: BTreeMap<String, String> = sources
        .par_iter()
        .filter_map(|src| hash_workspace_source(&workspace_root.join(src), workspace_root))
        .collect::<io::Result<_>>()?;
    Ok((!hashed.is_empty()).then_some(hashed))
}

/// Hash one source, yielding `None` when it is gone or not workspace-local.
fn hash_workspace_source(
    path: &Path,
    workspace_root: &Path,
) -> Option<io::Result<(String, String)>> {
    let hash = || -> io::Result<Option<(String, String)>> {
        let Some(canon) = zccache_core::scan_entry::entry_if_present(path.canonicalize())
            .map_err(|e| with_path(path, e))?
        else {
            return Ok(None);
        };
        let Ok(rel) = canon.strip_prefix(workspace_root) else {
            return Ok(None);
        };
        if is_vendored_dep(rel) {
            return Ok(None);
        }
        let Some(bytes) = zccache_core::scan_entry::entry_if_present(std::fs::read(&canon))
            .map_err(|e| with_path(&canon, e))?
        else {
            return Ok(None);
        };
        let hash = kernal_api::hash::blake3_bytes(&bytes).to_hex().to_string();
        Ok(Some((path_to_unix(rel), hash)))
    };
    hash().transpose()
}

/// Read the `run-build-script-build-script-build.json` file in `fp_dir`
/// and emit an entry covering its `RerunIfChanged` local fingerprints.
fn process_run_build_script(
    fp_dir: &Path,
    dir_name: &str,
    workspace_root: &Path,
    manifest_dirs: &HashMap<String, PathBuf>,
) -> io::Result<Option<CrateEntry>> {
    let json_path = fp_dir.join("run-build-script-build-script-build.json");
    let content = std::fs::read_to_string(&json_path).map_err(|e| with_path(&json_path, e))?;
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Ok(None);
    };
    let Some(local) = value.get("local").and_then(|local| local.as_array()) else {
        return Ok(None);
    };

    // Find the package's manifest dir from the fingerprint dir name. The
    // dir name is `<package>-<16-hex-hash>`.
    let Some(manifest_dir) = strip_hash_suffix(dir_name).and_then(|pkg| manifest_dirs.get(pkg))
    else {
        return Ok(None);
    };

    let mut sources = BTreeMap::new();
    let mut stamp_targets: Vec<String> = Vec::new();
    for item in local {
        let Some(rerun) = item.get("RerunIfChanged") else {
            continue;
        };
        let output = rerun
            .get("output")
            .and_then(|v| v.as_str())
            .map(|s| s.replace('\\', "/"));
        if let Some(out) = output {
            stamp_targets.push(out);
        }
        let Some(paths) = rerun.get("paths").and_then(|p| p.as_array()) else {
            continue;
        };
        for p in paths {
            let Some(rel) = p.as_str() else { continue };
            if let Some(hashed) = hash_workspace_source(&manifest_dir.join(rel), workspace_root) {
                let (rel, hash) = hashed?;
                sources.insert(rel, hash);
            }
        }
    }

    if sources.is_empty() || stamp_targets.is_empty() {
        return Ok(None);
    }
    Ok(Some(CrateEntry {
        fingerprint_dir: dir_name.to_string(),
        stamp_targets,
        sources,
    }))
}

/// `<crate>-<hex16>` → `Some("<crate>")`. Returns `None` if the suffix
/// isn't a plausible cargo fingerprint hash.
fn strip_hash_suffix(s: &str) -> Option<&str> {
    let idx = s.rfind('-')?;
    let (name, hash) = s.split_at(idx);
    let hash = &hash[1..];
    if hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(name)
    } else {
        None
    }
}

/// Walk `workspace_root` for `Cargo.toml` files and build a
/// `package_name → manifest_dir` map. Skips `target/`, `.cargo/`, `.git/`,
/// `node_modules/`, and dot-directories.
///
/// Fails on any walk error rather than return a partial map. A dedicated pool
/// keeps load on the shared one from ending this one-shot walk early.
fn discover_workspace_crates(workspace_root: &Path) -> io::Result<HashMap<String, PathBuf>> {
    let mut map: HashMap<String, PathBuf> = HashMap::new();
    // The facade prunes directories rather than filtering every child, so a
    // *file* with one of these names is no longer dropped here. That changes
    // nothing: the loop below keeps only `Cargo.toml`, so such a file was
    // discarded either way.
    let walker = kernal_api::platform::fs::DirectoryWalk::new(workspace_root.to_path_buf())
        .include_hidden_entries(true)
        .parallelism(
            kernal_api::platform::fs::DirectoryWalkParallelism::DedicatedPool { threads: 0 },
        )
        .prune_directories(|directory| {
            directory
                .file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| {
                    !matches!(
                        name,
                        "target" | ".cargo" | ".git" | "node_modules" | ".venv"
                    )
                })
        });
    for entry in zccache_core::scan_entry::walk_reporting_unreadable(walker) {
        let entry = entry?;
        let path = entry.path().to_path_buf();
        if path.file_name() != Some(std::ffi::OsStr::new("Cargo.toml")) {
            continue;
        }
        let Some(pkg_name) = parse_package_name(&path) else {
            continue;
        };
        let Some(dir) = path.parent() else { continue };
        map.insert(pkg_name, dir.to_path_buf());
    }
    Ok(map)
}

/// Minimal TOML parser that pulls only `[package].name = "..."`. Avoids
/// adding a `toml` dep for a one-field lookup. Returns `None` if the file
/// has no `[package]` section, no `name` key, or uses workspace inheritance
/// (`name.workspace = true`).
fn parse_package_name(toml_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(toml_path).ok()?;
    let mut in_package = false;
    for raw in content.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some(rest) = line.strip_prefix("name") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            // `name.workspace = true` lands here — workspace inheritance
            // we don't resolve, so skip.
            continue;
        };
        let val = rest.trim();
        let val = val.trim_matches(|c: char| c == '"' || c == '\'');
        if !val.is_empty() {
            return Some(val.to_string());
        }
    }
    None
}

/// Read the manifest written by `record` and selectively bump
/// `stamp_targets` mtimes for crates whose tracked sources still match.
pub fn validate(
    target_dir: &Path,
    workspace_root: &Path,
    manifest_path: &Path,
    _profile: &str,
    stamp_seconds_ahead: u64,
) -> io::Result<ValidateStats> {
    let manifest_bytes = match std::fs::read(manifest_path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ValidateStats::default()),
        Err(e) => return Err(e),
    };
    let manifest: Manifest = match serde_json::from_slice(&manifest_bytes) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(
                path = %manifest_path.display(),
                "manifest parse failed, leaving fingerprints alone: {e}"
            );
            return Ok(ValidateStats::default());
        }
    };
    if manifest.version != MANIFEST_VERSION {
        tracing::warn!(
            recorded = manifest.version,
            current = MANIFEST_VERSION,
            "manifest version mismatch, leaving fingerprints alone",
        );
        return Ok(ValidateStats::default());
    }

    let workspace_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let target_dir = target_dir
        .canonicalize()
        .unwrap_or_else(|_| target_dir.to_path_buf());
    let stamp_time = SystemTime::now() + Duration::from_secs(stamp_seconds_ahead);
    let file_times = FileTimes::new()
        .set_modified(stamp_time)
        .set_accessed(stamp_time);

    let results: Vec<bool> = manifest
        .crates
        .par_iter()
        .map(|entry| {
            let all_unchanged = entry.sources.par_iter().all(|(rel, expected_hex)| {
                let abs = workspace_root.join(rel);
                match std::fs::read(&abs) {
                    Ok(bytes) => kernal_api::hash::blake3_bytes(&bytes).to_hex() == *expected_hex,
                    Err(_) => false,
                }
            });
            if all_unchanged {
                for stamp_rel in &entry.stamp_targets {
                    let stamp_abs = target_dir.join(stamp_rel);
                    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&stamp_abs) {
                        let _ = file.set_times(file_times);
                    }
                }
            }
            all_unchanged
        })
        .collect();

    Ok(ValidateStats {
        crates_total: manifest.crates.len(),
        crates_clean: results.iter().filter(|c| **c).count(),
    })
}

#[derive(Debug, Default)]
pub struct ValidateStats {
    pub crates_total: usize,
    pub crates_clean: usize,
}

impl ValidateStats {
    pub fn crates_dirty(&self) -> usize {
        self.crates_total - self.crates_clean
    }
}

/// Parse rustc's Makefile-format `.d` dep file. Format:
///
/// ```text
/// <output_path>: <dep1> <dep2> <dep3...>
/// ```
///
/// Spaces inside paths are backslash-escaped (`\ `).
fn parse_d_file(content: &str) -> Vec<PathBuf> {
    let mut out = std::collections::HashSet::<PathBuf>::new();
    for line in content.lines() {
        let Some(idx) = line.find(": ") else { continue };
        let deps_str = &line[idx + 2..];
        let mut buf = String::new();
        let mut chars = deps_str.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' && chars.peek().map(|&n| n == ' ').unwrap_or(false) {
                buf.push(' ');
                chars.next();
            } else if c.is_whitespace() {
                if !buf.is_empty() {
                    out.insert(PathBuf::from(&buf));
                    buf.clear();
                }
            } else {
                buf.push(c);
            }
        }
        if !buf.is_empty() {
            out.insert(PathBuf::from(&buf));
        }
    }
    out.into_iter().collect()
}

/// Path → forward-slash UTF-8 string. The manifest must be cross-platform.
/// Strips the Windows `\\?\` extended-length prefix that `canonicalize`
/// emits.
fn path_to_unix(p: &Path) -> String {
    let s = p.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.replace('\\', "/")
}

/// True if `rel` (workspace-relative) points into a cargo registry or git
/// vendored-dep tree. Those are pinned by `Cargo.lock` and don't drift
/// between snapshot save and restore.
fn is_vendored_dep(rel: &Path) -> bool {
    let s = path_to_unix(rel);
    s.starts_with(".cargo/registry/")
        || s.starts_with(".cargo/git/")
        || s.contains("/.cargo/registry/")
        || s.contains("/.cargo/git/")
}

/// Best-effort timestamp without pulling chrono in as a dep.
fn chrono_like_now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch:{secs}")
}

#[cfg(test)]
mod tests;
