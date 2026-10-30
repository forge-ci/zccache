//! Bind unsigned imported PCH bytes to the parent PCH that consumed them.

use super::super::*;
use crate::fingerprint::pch_inputs::{inputs_stat_match, parse_pch_inputs, PchInput};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::io::{Read, Write};

pub(super) const SIDECAR_SUFFIX: &str = ".zccache-imports.json";

#[derive(Serialize, Deserialize)]
struct Provenance {
    version: u32,
    pch_blake3: String,
    imports: Vec<Import>,
}

#[derive(Serialize, Deserialize)]
struct Import {
    path: String,
    blake3: String,
}

fn invalid(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("PCH import provenance: {error}"),
    )
}

pub(super) fn sidecar_path(pch: &Path) -> NormalizedPath {
    let mut path = pch.as_os_str().to_owned();
    path.push(SIDECAR_SUFFIX);
    NormalizedPath::new(Path::new(&path))
}

fn imports(bytes: &[u8]) -> std::io::Result<Vec<PchInput>> {
    Ok(parse_pch_inputs(bytes)
        .map_err(invalid)?
        .unwrap_or_default()
        .into_iter()
        .filter(|input| input.imported_pch)
        .collect())
}

/// Capture a successful producer's imported bytes before its private view dies.
#[cfg(test)]
pub(super) fn record_pch_imports(
    pch: &Path,
    overlay_path: Option<&Path>,
) -> std::io::Result<Option<NormalizedPath>> {
    if overlay_path.is_some() {
        return record_pch_imports_for_output(pch, pch, overlay_path);
    }
    // These fixtures have just compiled against known, unchanged imports.
    // Production producers freeze these snapshots before executing clang.
    let mut plan = crate::fingerprint::pch_inputs::PchServePlan::default();
    for mut input in imports(&std::fs::read(pch)?)? {
        input.content_xxh3 = Some(crate::fingerprint::pch_inputs::content_xxh3(
            &std::fs::read(&input.path)?,
        ));
        plan.require_snapshot(&input).map_err(invalid)?;
    }
    let directory = tempfile::tempdir()?;
    let overlay = crate::fingerprint::pch_inputs::write_pch_overlay(&plan, directory.path())
        .map_err(invalid)?;
    record_pch_imports_for_output(pch, pch, Some(&overlay))
}

/// The compiler may have written the primary PCH into a private staging root.
/// Bind those exact bytes and publish the companion beside the logical output.
pub(in crate::daemon::server) fn record_pch_imports_for_output(
    pch: &Path,
    logical_output: &Path,
    overlay_path: Option<&Path>,
) -> std::io::Result<Option<NormalizedPath>> {
    let bytes = std::fs::read(pch)?;
    let records = imports(&bytes)?;
    if records.is_empty() {
        match kernal_api::platform::fs::remove_file(sidecar_path(logical_output)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        return Ok(None);
    }
    let mut mappings = HashMap::new();
    if let Some(overlay) = overlay_path {
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(overlay)?).map_err(invalid)?;
        let roots = document["roots"]
            .as_array()
            .ok_or_else(|| invalid("overlay roots missing"))?;
        for root in roots {
            let name = root["name"]
                .as_str()
                .ok_or_else(|| invalid("overlay name missing"))?;
            let external = root["external-contents"]
                .as_str()
                .ok_or_else(|| invalid("overlay snapshot missing"))?;
            mappings.insert(NormalizedPath::new(name), NormalizedPath::new(external));
        }
    }
    let imports = records
        .into_iter()
        .map(|record| {
            let key = NormalizedPath::new(&record.path);
            let visible = mappings.get(&key).ok_or_else(|| {
                invalid(format!("executed import snapshot missing: {}", record.path))
            })?;
            let mut file = std::fs::File::open(visible)?;
            let metadata = file.metadata()?;
            let seconds =
                kernal_api::platform::fs::FileTime::from_last_modification_time(&metadata)
                    .unix_seconds();
            if metadata.len() != record.size
                || (record.mtime_secs != 0
                    && u64::try_from(seconds).ok() != Some(record.mtime_secs))
            {
                return Err(invalid(format!(
                    "import changed before producer provenance capture: {}",
                    record.path
                )));
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            let after = file.metadata()?;
            if bytes.len() as u64 != record.size || after.modified()? != metadata.modified()? {
                return Err(invalid(format!(
                    "import changed during producer provenance capture: {}",
                    record.path
                )));
            }
            Ok(Import {
                path: record.path,
                blake3: crate::hash::hash_bytes(&bytes).to_hex(),
            })
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    let provenance = Provenance {
        version: 1,
        pch_blake3: crate::hash::hash_bytes(&bytes).to_hex(),
        imports,
    };
    let destination = sidecar_path(logical_output);
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("sidecar has no parent directory"))?;
    kernal_api::platform::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(temporary.as_file_mut(), &provenance).map_err(invalid)?;
    temporary.as_file_mut().flush()?;
    temporary
        .persist(&destination)
        .map_err(|error| error.error)?;
    Ok(Some(destination))
}

fn validate(bytes: &[u8], sidecar: &[u8], records: &[PchInput]) -> std::io::Result<Provenance> {
    let provenance: Provenance = serde_json::from_slice(sidecar).map_err(invalid)?;
    if provenance.version != 1 || provenance.pch_blake3 != crate::hash::hash_bytes(bytes).to_hex() {
        return Err(invalid("parent bytes do not match the recorded producer"));
    }
    if provenance.imports.len() != records.len()
        || records.iter().any(|record| {
            !provenance
                .imports
                .iter()
                .any(|import| import.path == record.path)
        })
    {
        return Err(invalid("import list does not match parent records"));
    }
    Ok(provenance)
}

/// Existing artifact packaging owns this companion; it is not a separate cache.
pub(super) fn cached_import_sidecar(
    pch: &Path,
    bytes: &[u8],
) -> std::io::Result<Option<(NormalizedPath, Vec<u8>)>> {
    let records = imports(bytes)?;
    if records.is_empty() {
        return Ok(None);
    }
    let path = sidecar_path(pch);
    let sidecar = std::fs::read(&path)?;
    validate(bytes, &sidecar, &records)?;
    Ok(Some((NormalizedPath::new(path), sidecar)))
}

/// Detached publication must read immutable bytes from this compile's owned
/// directory, since another producer may replace the logical companion.
pub(super) fn preserve_import_sidecar(
    logical: &Path,
    bytes: &[u8],
    owner: &mut Option<tempfile::TempDir>,
    temporary_root: &Path,
) -> std::io::Result<NormalizedPath> {
    if owner.is_none() {
        *owner = Some(
            tempfile::Builder::new()
                .prefix("pch-provenance-persist-")
                .tempdir_in(temporary_root)?,
        );
    }
    let directory = owner
        .as_ref()
        .ok_or_else(|| invalid("private companion owner missing"))?;
    let name = logical
        .file_name()
        .ok_or_else(|| invalid("sidecar filename missing"))?;
    let path = NormalizedPath::new(directory.path()).join(name);
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    file.write_all(bytes)?;
    Ok(NormalizedPath::new(path))
}

fn verify_current(
    provenance: &Provenance,
    visited: &mut std::collections::HashSet<NormalizedPath>,
    verified: &mut HashMap<String, u64>,
    require_bound: bool,
) -> std::io::Result<()> {
    for import in &provenance.imports {
        let path = Path::new(&import.path);
        let bytes = std::fs::read(path)?;
        if crate::hash::hash_bytes(&bytes).to_hex() != import.blake3 {
            return Err(invalid(format!(
                "imported bytes changed: {}",
                path.display()
            )));
        }
        verified.insert(
            import.path.clone(),
            crate::fingerprint::pch_inputs::content_xxh3(&bytes),
        );
        verify_loaded(path, &bytes, visited, verified, require_bound)?;
    }
    Ok(())
}

fn verify_loaded(
    pch: &Path,
    bytes: &[u8],
    visited: &mut std::collections::HashSet<NormalizedPath>,
    verified: &mut HashMap<String, u64>,
    require_bound: bool,
) -> std::io::Result<()> {
    if !visited.insert(NormalizedPath::new(pch)) {
        return Ok(());
    }
    let records = imports(bytes)?;
    if records.is_empty() {
        return Ok(());
    }
    match std::fs::read(sidecar_path(pch)) {
        Ok(sidecar) => verify_current(
            &validate(bytes, &sidecar, &records)?,
            visited,
            verified,
            require_bound,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if require_bound {
                return Err(invalid(format!(
                    "producer baseline missing for private PCH view: {}",
                    pch.display()
                )));
            }
            // A legacy PCH may use native checks when no private view is
            // needed. Matching stats do not establish content provenance:
            // never turn current imported bytes into an expected hash.
            if !inputs_stat_match(&records) {
                return Err(invalid(
                    "producer baseline missing for changed imported metadata",
                ));
            }
            for record in records {
                let path = Path::new(&record.path);
                let bytes = std::fs::read(path)?;
                verify_loaded(path, &bytes, visited, verified, false)?;
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Verify loaded PCHs recursively. Never invent a baseline from current bytes.
#[cfg(test)]
pub(super) fn verify_pch_imports(pch: &Path) -> std::io::Result<()> {
    verified_pch_import_hashes(pch).map(|_| ())
}

/// Freeze each imported identity from the same bytes that pass provenance
/// validation. Snapshot creation must recheck these xxh3 values.
pub(super) fn verified_pch_import_hashes(pch: &Path) -> std::io::Result<HashMap<String, u64>> {
    import_hashes(pch, true)
}

/// Preflight a native legacy consumer without authorizing private repair.
/// Imports lacking bound provenance deliberately receive no expected hash.
pub(super) fn legacy_pch_import_hashes(pch: &Path) -> std::io::Result<HashMap<String, u64>> {
    import_hashes(pch, false)
}

fn import_hashes(pch: &Path, require_bound: bool) -> std::io::Result<HashMap<String, u64>> {
    let mut verified = HashMap::new();
    verify_loaded(
        pch,
        &std::fs::read(pch)?,
        &mut std::collections::HashSet::new(),
        &mut verified,
        require_bound,
    )?;
    Ok(verified)
}

fn payload_bytes(payload: &CachedPayload) -> std::io::Result<Cow<'_, [u8]>> {
    match payload {
        CachedPayload::Bytes(bytes) => Ok(Cow::Borrowed(bytes.as_slice())),
        CachedPayload::File(path) => Ok(Cow::Owned(std::fs::read(path)?)),
    }
}

/// Every cached parent with imports needs its original producer baseline,
/// including old cache entries whose imported timestamps happen to match.
pub(super) fn verify_cached_imports(
    primary: &CachedPayload,
    manifest: Option<&CachedPayload>,
) -> std::io::Result<()> {
    let bytes = payload_bytes(primary)?;
    let records = imports(&bytes)?;
    if records.is_empty() {
        return Ok(());
    }
    let manifest = manifest.ok_or_else(|| invalid("cached producer baseline missing"))?;
    let sidecar = payload_bytes(manifest)?;
    let provenance = validate(&bytes, &sidecar, &records)?;
    verify_current(
        &provenance,
        &mut std::collections::HashSet::new(),
        &mut HashMap::new(),
        true,
    )
}

#[cfg(test)]
#[path = "pch_provenance_tests.rs"]
mod tests;
