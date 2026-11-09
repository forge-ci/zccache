//! Resolve bundled archives into the existing extern content-verification path (#1945).

use super::*;
use crate::depgraph::{ExternCrate, RustcParsedArgs, NATIVE_INPUT_PREFIX};

#[cfg(test)]
#[path = "native_inputs_tests.rs"]
mod tests;

/// Keep search-path kinds: dependency/crate/framework directories cannot
/// resolve a native archive. The general argument parser drops these kinds.
fn native_search_dirs(parsed: &RustcParsedArgs) -> Vec<NormalizedPath> {
    parsed
        .search_paths
        .iter()
        .filter(|path| path.kind == "native" || path.kind == "all")
        .map(|path| path.path.clone())
        .collect()
}

fn archive_name(spec: &str) -> Result<Option<(&str, bool)>, &'static str> {
    let Some((kind, name)) = spec.split_once('=') else {
        return Ok(Some((spec, false)));
    };
    let (kind, modifiers) = kind.split_once(':').unwrap_or((kind, ""));
    match kind {
        "dylib" | "framework" => return Ok(None),
        "static" => {}
        _ => return Err("unsupported native library kind"),
    }
    if !modifiers.is_empty()
        && modifiers.split(',').any(|m| {
            !matches!(
                m,
                "+bundle" | "-bundle" | "+whole-archive" | "-whole-archive"
            )
        })
    {
        return Err("unsupported native library modifier");
    }
    Ok(Some((name, true)))
}

fn resolve_archive(
    name: &str,
    static_only: bool,
    dirs: &[NormalizedPath],
    cwd: &Path,
    msvc: bool,
) -> Result<NormalizedPath, &'static str> {
    if name.is_empty() || name.contains(['/', '\\', ':']) {
        return Err("unsupported native archive name");
    }
    // Reuse the link resolver's ordered directory walk and exact-file mode.
    // On MSVC rustc also accepts name.lib. Refuse coexisting spellings rather
    // than guessing which toolchain-specific candidate rustc would choose.
    let names = if msvc {
        vec![format!(":{name}.lib"), format!(":lib{name}.a")]
    } else {
        vec![format!(":lib{name}.a")]
    };
    for dir in dirs {
        let search = std::slice::from_ref(dir);
        if !static_only
            && [format!(":lib{name}.so"), format!(":lib{name}.dylib")]
                .iter()
                .any(|n| resolve_link_library(n, search, cwd).is_some())
        {
            return Err("ambiguous default native library kind");
        }
        let mut found = None;
        for candidate in &names {
            if let Some(path) = resolve_link_library(candidate, search, cwd) {
                if found.replace(path).is_some() {
                    return Err("ambiguous native archive spelling");
                }
            }
        }
        if let Some(path) = found {
            return Ok(path);
        }
    }
    Err("native archive not resolved in explicit native search paths")
}

/// Unlike output discovery, archive selection cannot overapproximate a
/// Windows host as MSVC: Windows-GNU may select a different file. Without
/// an explicit target, accept only an archive both ABI models agree on.
fn resolve_target_archive(
    name: &str,
    static_only: bool,
    dirs: &[NormalizedPath],
    cwd: &Path,
    target: Option<&str>,
    host_windows: bool,
) -> Result<NormalizedPath, &'static str> {
    if target.is_some_and(|target| target.ends_with(".json")) {
        return Err("custom target archive naming is not modeled");
    }
    let msvc = target.map_or(host_windows, |target| target.ends_with("-msvc"));
    let path = resolve_archive(name, static_only, dirs, cwd, msvc)?;
    if target.is_none()
        && host_windows
        && resolve_archive(name, static_only, dirs, cwd, false)? != path
    {
        return Err("host native archive ABI is ambiguous");
    }
    Ok(path)
}

/// Synthetic extern names are internal key material, never compiler argv.
/// Reusing externs covers fast hits, request hits, depgraph hits and stores.
fn inputs(
    parsed: &RustcParsedArgs,
    _raw: &[String],
    cwd: &Path,
) -> Result<Vec<ExternCrate>, &'static str> {
    let libraries: Vec<_> = parsed
        .unknown_flags
        .iter()
        .filter_map(|flag| flag.strip_prefix("-l "))
        .collect();
    let dirs = native_search_dirs(parsed);
    if !dirs.is_empty()
        && parsed
            .target
            .as_deref()
            .is_some_and(|target| target.ends_with(".json"))
    {
        return Err("custom target archive naming is not modeled");
    }
    let mut inputs = Vec::new();
    for (index, spec) in libraries.into_iter().enumerate() {
        let Some((name, static_only)) = archive_name(spec)? else {
            continue;
        };
        inputs.push(ExternCrate {
            name: format!("{NATIVE_INPUT_PREFIX}{index}:{spec}"),
            path: resolve_target_archive(
                name,
                static_only,
                &dirs,
                cwd,
                parsed.target.as_deref(),
                crate::platform::host::is_windows(),
            )?,
        });
    }
    // Source #[link(kind="static")] can bundle an archive in an rlib
    // without any argv -l. Use the same native input owner for every product.
    add_transitive_archives(&dirs, cwd, &mut inputs)?;
    Ok(inputs)
}

/// A plain `-l foo` or `static:-bundle=foo` can defer linking to a downstream
/// binary without changing the rlib bytes. Cargo forwards its native search
/// directories, but not its `-l` flags, to that binary. Track every static
/// archive in those explicit directories rather than guessing which extern
/// introduced it. This deliberately does not search sysroots or implicit paths.
fn add_transitive_archives(
    dirs: &[NormalizedPath],
    cwd: &Path,
    inputs: &mut Vec<ExternCrate>,
) -> Result<(), &'static str> {
    for (index, dir) in dirs.iter().enumerate() {
        let directory = link_path_to_absolute(dir.as_path(), cwd);
        let mut paths = Vec::new();
        for entry in
            std::fs::read_dir(directory).map_err(|_| "unreadable native search directory")?
        {
            let path = entry
                .map_err(|_| "unreadable native directory entry")?
                .path();
            let library_extension = path.extension().is_some_and(|ext| {
                ext.to_str().is_some_and(|ext| {
                    ["a", "lib", "so", "dylib", "dll", "tbd"]
                        .iter()
                        .any(|known| ext.eq_ignore_ascii_case(known))
                })
            });
            let versioned_shared = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".so."));
            if !library_extension && !versioned_shared {
                continue;
            }
            if !path
                .metadata()
                .map_err(|_| "unreadable native archive")?
                .is_file()
            {
                continue;
            }
            paths.push(NormalizedPath::new(path));
        }
        paths.sort();
        for path in paths {
            if inputs.iter().any(|input| input.path == path) {
                continue;
            }
            let name = path
                .as_path()
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("unsupported native archive filename")?;
            inputs.push(ExternCrate {
                name: format!("{NATIVE_INPUT_PREFIX}search:{index}:{name}"),
                path,
            });
        }
    }
    Ok(())
}

pub(super) fn add_inputs(parsed: &mut RustcParsedArgs, raw: &[String], cwd: &Path) -> bool {
    match inputs(parsed, raw, cwd) {
        Ok(inputs) => {
            parsed.externs.extend(inputs);
            true
        }
        Err(reason) => {
            tracing::debug!(reason, "native input resolution refused caching");
            false
        }
    }
}

pub(super) async fn add_inputs_async(
    parsed: &mut RustcParsedArgs,
    raw: &[String],
    cwd: &Path,
) -> bool {
    if !parsed
        .unknown_flags
        .iter()
        .any(|flag| flag.starts_with("-l "))
        && native_search_dirs(parsed).is_empty()
    {
        return true;
    }
    let raw = raw.to_vec();
    let cwd = cwd.to_path_buf();
    match kernal_api::async_engine::launch_blocking(move || {
        let parsed = crate::depgraph::parse_rustc_args(&raw, &cwd);
        inputs(&parsed, &raw, &cwd)
    })
    .await
    {
        Ok(Ok(inputs)) => {
            parsed.externs.extend(inputs);
            true
        }
        _ => false,
    }
}
