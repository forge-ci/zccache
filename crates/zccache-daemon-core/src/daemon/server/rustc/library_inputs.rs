//! Resolve rustc metadata's named transitive inputs before every cache lookup.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use rayon::prelude::*;

use crate::core::NormalizedPath;
use crate::depgraph::{ExternCrate, NATIVE_INPUT_PREFIX};
use crate::fscache::MetadataCache;
use crate::hash::ContentHash;

use super::super::SharedState;
use super::library_metadata::{self, Dependency};
use super::BuildContextResult;

const INPUT_PREFIX: &str = "__zccache_rust_library__:";
const INVENTORY_MIN_EXTERNS: usize = 8;

fn candidates(stem: &str, dirs: &[NormalizedPath]) -> Result<Vec<NormalizedPath>, &'static str> {
    let mut selected = Vec::new();
    let mut selected_directory = None;
    for dir in dirs {
        let mut files = Vec::new();
        for name in [
            format!("lib{stem}.rmeta"),
            format!("lib{stem}.rlib"),
            format!("lib{stem}.so"),
            format!("lib{stem}.dylib"),
            format!("{stem}.dll"),
            format!("lib{stem}.dll"),
            format!("{stem}.dll.lib"),
            format!("lib{stem}.dll.lib"),
            format!("lib{stem}.dll.a"),
        ] {
            let path = dir.join(name);
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => files.push(path),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("unreadable Rust library candidate"),
            }
        }
        if files.is_empty() {
            continue;
        }
        if selected_directory
            .as_ref()
            .is_some_and(|previous| previous != dir)
        {
            return Err("ambiguous Rust dependency search");
        }
        selected_directory = Some(dir.clone());
        selected.extend(files);
    }
    selected.sort();
    selected.dedup();
    if selected.is_empty() {
        return Err("unresolved transitive Rust library");
    }
    Ok(selected)
}

fn resolve(
    dependency: &Dependency,
    dirs: &[NormalizedPath],
    compiler: &NormalizedPath,
    compiler_hash: ContentHash,
    files: &MetadataCache,
    probe_dir: &Path,
) -> Result<Vec<NormalizedPath>, &'static str> {
    let candidates = candidates(&dependency.stem, dirs)?;
    // Prefer metadata for inspection: shipped sysroot rlibs may deliberately
    // omit their embedded metadata. Hash all co-located flavors below because
    // rustc can read rmeta for type checking and rlib/dylib bytes for codegen.
    let selected = candidates
        .iter()
        .find(|path| path.extension().is_some_and(|ext| ext == "rmeta"))
        .or_else(|| {
            candidates
                .iter()
                .find(|path| path.extension().is_some_and(|ext| ext == "rlib"))
        })
        .or_else(|| {
            candidates.iter().find(|path| {
                path.extension()
                    .is_some_and(|ext| matches!(ext.to_str(), Some("so" | "dylib" | "dll")))
            })
        })
        .ok_or("missing dependency metadata")?;
    let metadata = library_metadata::inspect(compiler, compiler_hash, selected, files, probe_dir)?;
    if metadata.stem != dependency.stem || metadata.hash != dependency.hash {
        return Err("transitive Rust crate identity does not match");
    }
    Ok(candidates)
}

fn inputs(
    externs: &[ExternCrate],
    dirs: &[NormalizedPath],
    compiler: &NormalizedPath,
    compiler_hash: ContentHash,
    files: &MetadataCache,
    probe_dir: &Path,
) -> Result<Vec<ExternCrate>, &'static str> {
    // Amortize the default sysroot inventory only across a large fanout. Small
    // graphs and explicit targets retain exact discovery and relocation reuse.
    if externs.len() >= INVENTORY_MIN_EXTERNS {
        for external in externs {
            if external.path.extension().is_some_and(|extension| {
                extension
                    .to_str()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("rlib"))
            }) {
                let hash = files
                    .lookup_normalized(&external.path)
                    .map_err(|_| "unreadable Rust library")?;
                library_metadata::regular_archive(&external.path, hash, probe_dir)?;
            }
        }
        if let Some(inputs) =
            super::library_inventory::inputs(dirs, compiler, compiler_hash, files, probe_dir)?
        {
            return Ok(inputs);
        }
    }
    named_inputs(externs, dirs, compiler, compiler_hash, files, probe_dir)
}

fn named_inputs(
    externs: &[ExternCrate],
    dirs: &[NormalizedPath],
    compiler: &NormalizedPath,
    compiler_hash: ContentHash,
    files: &MetadataCache,
    probe_dir: &Path,
) -> Result<Vec<ExternCrate>, &'static str> {
    let mut resolved = Vec::new();
    let roots = externs
        .par_iter()
        .map(|external| {
            library_metadata::inspect(compiler, compiler_hash, &external.path, files, probe_dir)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut inspected = HashSet::new();
    for metadata in &roots {
        // Each root carries the complete closure. Workspace links often repeat
        // the same sysroot crates in dozens of roots: verify each identity once
        // per request, while still rechecking directory membership on every hit.
        let dependencies = metadata
            .dependencies
            .iter()
            .filter(|dependency| {
                inspected.insert((
                    metadata.triple.as_str(),
                    dependency.stem.as_str(),
                    dependency.hash,
                ))
            })
            .collect::<Vec<_>>();
        if dependencies.is_empty() {
            continue;
        }
        let sysroot = library_metadata::target_directory(
            compiler,
            compiler_hash,
            Some(&metadata.triple),
            probe_dir,
        )?;
        let mut search = dirs.to_vec();
        if !search.contains(&sysroot) {
            search.push(sysroot);
        }
        // rustc's crate-number table includes the complete external crate
        // closure, not only the producer's direct --extern declarations.
        let groups = dependencies
            .par_iter()
            .map(|dependency| {
                // An unused explicit alias need not participate in rustc's crate
                // lookup. Resolve each named dependency through its search paths.
                resolve(
                    dependency,
                    &search,
                    compiler,
                    compiler_hash,
                    files,
                    probe_dir,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        for paths in groups {
            for path in paths {
                if path
                    .extension()
                    .is_some_and(|extension| extension == "rlib")
                {
                    super::native_inputs::require_regular_archive(path.as_path())?;
                }
                if resolved
                    .iter()
                    .any(|input: &ExternCrate| input.path == path)
                {
                    continue;
                }
                let hash = files
                    .lookup_normalized(&path)
                    .map_err(|_| "unreadable transitive Rust input")?;
                let filename = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("invalid Rust library filename")?;
                // Digest identity prevents concurrent same-basename inputs
                // sharing a dependency mapping, while preserving relocation
                // across target directories/worktrees with identical bytes.
                resolved.push(ExternCrate {
                    name: format!("{INPUT_PREFIX}{filename}:{hash}"),
                    path,
                });
            }
        }
    }
    resolved.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(resolved)
}

/// Resolution runs on the blocking lane, without retaining any data guard.
/// The normal extern-input machinery then verifies these paths on all hit and
/// publication lanes, and the daemon watcher tracks them like explicit inputs.
pub(in crate::daemon::server) async fn complete_context(
    state: &Arc<SharedState>,
    compilation: &crate::compiler::CacheableCompilation,
    result: &mut BuildContextResult,
    cwd: &Path,
) -> bool {
    let BuildContextResult::Rustc {
        rustc_args,
        rustc_ctx,
        ..
    } = result
    else {
        return true;
    };
    let externs: Vec<_> = rustc_args
        .externs
        .iter()
        .filter(|input| !input.name.starts_with(NATIVE_INPUT_PREFIX))
        .cloned()
        .collect();
    if externs.is_empty() {
        return true;
    }
    let dirs = rustc_args
        .search_paths
        .iter()
        .filter(|path| path.kind == "dependency" || path.kind == "all")
        .map(|path| path.path.clone())
        .collect::<Vec<_>>();
    let compiler = super::rustc_identity_path(compilation, cwd);
    let compiler_hash = rustc_ctx.compiler_hash;
    let inventory_link = rustc_args.crate_types == ["staticlib"]
        && rustc_args.target.is_none()
        && !rustc_args.emit_types.iter().any(|kind| kind == "metadata")
        && !super::library_inventory::candidate(compilation.output_file.as_path());
    let state = Arc::clone(state);
    let resolved = kernal_api::async_engine::launch_blocking(move || {
        let files = state.cache_system.metadata();
        let probe_dir = state.depfile_tmpdir.as_path();
        if inventory_link {
            inputs(&externs, &dirs, &compiler, compiler_hash, files, probe_dir)
        } else {
            named_inputs(&externs, &dirs, &compiler, compiler_hash, files, probe_dir)
        }
    })
    .await;
    match resolved {
        Ok(Ok(inputs)) => {
            for input in inputs {
                rustc_ctx.extern_crates.push((
                    input.name.clone(),
                    input.path.to_string_lossy().into_owned(),
                ));
                rustc_args.externs.push(input);
            }
            rustc_ctx.extern_crates.sort();
            true
        }
        failure => {
            tracing::debug!(
                ?failure,
                "transitive Rust library resolution refused caching"
            );
            false
        }
    }
}

#[cfg(test)]
#[path = "library_inputs_tests.rs"]
mod tests;
