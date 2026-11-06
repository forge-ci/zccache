//! Response-file (`@file`) expansion + caching.
//!
//! Compilations frequently pass a sequence of `@response.rsp` arguments that
//! must be expanded into their constituent flags before hashing. We cache the
//! expansion keyed by the canonical path, invalidating when any input file's
//! content hash changes.

use super::*;
use crate::compiler::response_file::ResponseFileError;

#[derive(Clone)]
pub(super) struct RspDependency {
    pub(super) path: NormalizedPath,
    pub(super) hash: ContentHash,
}

#[derive(Clone)]
pub(super) struct RspCacheEntry {
    pub(super) expanded: Vec<String>,
    pub(super) dependencies: Vec<RspDependency>,
    pub(super) cached_at: std::time::Instant,
    /// Family whose grammar produced `expanded`; rustc splits a file
    /// differently from the GCC family, so another family re-expands it.
    pub(super) family: crate::compiler::CompilerFamily,
}

/// Expand response file references with caching.
///
/// For each `@file` argument, checks if the expansion is already cached.
/// If so, uses the cached result (no file I/O or canonicalize). Otherwise,
/// expands the reference and caches the result for future requests.
/// Non-`@file` arguments are passed through unchanged. Files are read the
/// way a compiler of `family` reads them.
pub(super) fn expand_args_cached(
    state: &SharedState,
    args: &[String],
    cwd: &Path,
    family: crate::compiler::CompilerFamily,
) -> Result<Vec<String>, ResponseFileError> {
    let expand_bare_at = family == crate::compiler::CompilerFamily::Rustc;
    // Quick check: skip expansion if no @file references exist
    if !args
        .iter()
        .any(|a| a.starts_with('@') && (a.len() > 1 || expand_bare_at))
    {
        return Ok(args.to_vec());
    }

    let mut result = Vec::with_capacity(args.len());
    for arg in args {
        if arg.starts_with('@') && (arg.len() > 1 || expand_bare_at) {
            let filename = &arg[1..];
            let resolved: NormalizedPath = if Path::new(filename).is_absolute() {
                filename.into()
            } else {
                cwd.join(filename).into()
            };

            match expand_rsp_arg_cached(state, &resolved, family) {
                Ok(expanded) => result.extend(expanded),
                Err(error) if expand_bare_at => return Err(error),
                Err(e) => {
                    tracing::debug!("response file expansion failed: {e}, passing raw arg");
                    result.push(arg.clone());
                }
            }
        } else {
            result.push(arg.clone());
        }
    }
    Ok(result)
}

pub(super) fn expand_rsp_arg_cached(
    state: &SharedState,
    resolved: &Path,
    family: crate::compiler::CompilerFamily,
) -> Result<Vec<String>, ResponseFileError> {
    let canonical: NormalizedPath = resolved
        .canonicalize()
        .map_err(|source| ResponseFileError::ReadError {
            path: resolved.into(),
            source,
        })?
        .into();

    if let Some(cached) = state.rsp_cache.get(&canonical) {
        let fresh = cached.family == family
            && cached
                .dependencies
                .iter()
                .all(|dep| hash_file_via_cache(state, &dep.path) == Some(dep.hash));
        if fresh {
            return Ok(cached.expanded.clone());
        }
    }

    let mut seen = HashSet::new();
    let mut dependencies = Vec::new();
    let expanded =
        expand_rsp_recursive(state, &canonical, &mut seen, &mut dependencies, 0, family)?;
    state.rsp_cache.insert(
        canonical,
        RspCacheEntry {
            expanded: expanded.clone(),
            dependencies,
            cached_at: std::time::Instant::now(),
            family,
        },
    );
    Ok(expanded)
}

pub(super) fn expand_rsp_recursive(
    state: &SharedState,
    path: &Path,
    seen: &mut HashSet<NormalizedPath>,
    dependencies: &mut Vec<RspDependency>,
    depth: usize,
    family: crate::compiler::CompilerFamily,
) -> Result<Vec<String>, crate::compiler::response_file::ResponseFileError> {
    use crate::compiler::response_file::{
        expands_nested_response_files, parse_response_file_content_for,
    };

    const MAX_RSP_DEPTH: usize = 10;

    if depth >= MAX_RSP_DEPTH {
        return Err(ResponseFileError::TooDeep { path: path.into() });
    }

    let canonical: NormalizedPath = path
        .canonicalize()
        .map_err(|e| ResponseFileError::ReadError {
            path: path.into(),
            source: e,
        })?
        .into();

    if !seen.insert(canonical.clone()) {
        return Err(ResponseFileError::CircularReference {
            path: canonical.clone(),
        });
    }

    let content_hash =
        hash_file_via_cache(state, &canonical).ok_or_else(|| ResponseFileError::ReadError {
            path: canonical.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "failed to hash response file",
            ),
        })?;
    dependencies.push(RspDependency {
        path: canonical.clone(),
        hash: content_hash,
    });

    // Keep daemon-side key expansion byte-for-byte aligned with the compiler
    // response-file reader. In particular, MSVC commonly emits UTF-16LE
    // response files; `read_to_string` rejected those and left the raw
    // `@file` invisible to parsing and cache keys.
    let content = crate::compiler::response_file::read_response_file_text_for(&canonical, family)
        .map_err(|e| ResponseFileError::ReadError {
        path: canonical.clone(),
        source: e,
    })?;
    let base_dir = canonical.parent().unwrap_or_else(|| Path::new("."));
    let expands_nested = expands_nested_response_files(family);
    let mut expanded = Vec::new();
    for child in parse_response_file_content_for(&content, family) {
        if let Some(filename) = child.strip_prefix('@').filter(|_| expands_nested) {
            if filename.is_empty() {
                expanded.push(child);
                continue;
            }
            let child_path: NormalizedPath = if Path::new(filename).is_absolute() {
                filename.into()
            } else {
                base_dir.join(filename).into()
            };
            expanded.extend(expand_rsp_recursive(
                state,
                &child_path,
                seen,
                dependencies,
                depth + 1,
                family,
            )?);
        } else {
            expanded.push(child);
        }
    }

    seen.remove(&canonical);
    Ok(expanded)
}
