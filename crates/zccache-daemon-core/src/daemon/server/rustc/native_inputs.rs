//! Resolve bundled archives into the existing extern content-verification path (#1945).

use super::*;
use crate::depgraph::{ExternCrate, RustcParsedArgs, NATIVE_INPUT_PREFIX};

#[cfg(test)]
#[path = "native_inputs_tests.rs"]
mod tests;

/// Keep search-path kinds: dependency/crate/framework directories cannot
/// resolve a native archive. The general argument parser drops these kinds.
fn native_search_dirs(raw: &[String], cwd: &Path) -> Vec<NormalizedPath> {
    let mut dirs = Vec::new();
    let mut args = raw.iter();
    while let Some(arg) = args.next() {
        let value = if arg == "-L" {
            args.next().map(String::as_str)
        } else {
            arg.strip_prefix("-L").filter(|value| !value.is_empty())
        };
        let Some(value) = value else { continue };
        let path = match value.split_once('=') {
            Some(("native" | "all", path)) => path,
            Some(_) => continue,
            None => value,
        };
        dirs.push(link_path_to_absolute(Path::new(path), cwd).into());
    }
    dirs
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

/// Synthetic extern names are internal key material, never compiler argv.
/// Reusing externs covers fast hits, request hits, depgraph hits and stores.
fn inputs(
    parsed: &RustcParsedArgs,
    raw: &[String],
    cwd: &Path,
) -> Result<Vec<ExternCrate>, &'static str> {
    let libraries: Vec<_> = parsed
        .unknown_flags
        .iter()
        .filter_map(|flag| flag.strip_prefix("-l "))
        .collect();
    if libraries.is_empty() && super::link_key::linked_product(parsed).is_none() {
        return Ok(Vec::new());
    }
    let dirs = native_search_dirs(raw, cwd);
    let msvc = super::msvc_target_writes_pdb(parsed);
    let mut inputs = Vec::new();
    for (index, spec) in libraries.into_iter().enumerate() {
        let Some((name, static_only)) = archive_name(spec)? else {
            continue;
        };
        inputs.push(ExternCrate {
            name: format!("{NATIVE_INPUT_PREFIX}{index}:{spec}"),
            path: resolve_archive(name, static_only, &dirs, cwd, msvc)?,
        });
    }
    if super::link_key::linked_product(parsed).is_some() {
        add_transitive_archives(&dirs, cwd, &mut inputs)?;
    }
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
            if !path.extension().is_some_and(|ext| {
                ext.to_str().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("a") || ext.eq_ignore_ascii_case("lib")
                })
            }) {
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
        && (super::link_key::linked_product(parsed).is_none()
            || native_search_dirs(raw, cwd).is_empty())
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
