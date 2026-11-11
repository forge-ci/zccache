//! Private depfile sources retained through detached publication.
use super::{
    bind_depfile_to_root, canonicalize_depfile_root, names_unrewritten_root_path, NormalizedPath,
    Path,
};

/// Store the captured user depfile with its key root made logical.
///
/// The requested depfile on disk keeps the compiler's bytes; the cached
/// payload comes from a private copy so a sibling worktree's hit can
/// rehydrate its own root (`rehydrate_delivered_depfile`). A depfile that
/// still names the root in a spelling the rewrite missed is bound to this
/// root, so a sibling's hit recompiles rather than replaying it.
pub(in crate::daemon::server::handle_compile) fn canonicalize_captured_depfile_root(
    capture: &mut Option<(NormalizedPath, Vec<u8>)>,
    persist_temp: Option<tempfile::TempDir>,
    key_root: &Path,
    temp_root: &Path,
) -> std::io::Result<Option<tempfile::TempDir>> {
    let Some((source_path, bytes)) = capture.as_mut() else {
        return Ok(persist_temp);
    };
    let mut canonical = canonicalize_depfile_root(bytes, key_root);
    if names_unrewritten_root_path(&canonical, key_root) {
        canonical = bind_depfile_to_root(&canonical, key_root);
    }
    if canonical == *bytes {
        return Ok(persist_temp);
    }
    *bytes = canonical;
    if persist_temp.is_some() {
        std::fs::write(source_path.as_path(), bytes.as_slice())?;
        return Ok(persist_temp);
    }
    let requested = source_path.clone();
    preserve_staged_depfile_for_persistence(capture, Some(&requested), temp_root)
}

/// Preserve canonical staged depfile bytes after requested-output
/// materialization removes the compiler's private staging root.
///
/// The returned directory is an ownership guard: callers must keep it alive
/// through synchronous persistence or move it into the detached persist task.
pub(in crate::daemon::server::handle_compile) fn preserve_staged_depfile_for_persistence(
    capture: &mut Option<(NormalizedPath, Vec<u8>)>,
    requested_path: Option<&NormalizedPath>,
    temp_root: &Path,
) -> std::io::Result<Option<tempfile::TempDir>> {
    let (Some((source_path, bytes)), Some(requested_path)) = (capture.as_mut(), requested_path)
    else {
        return Ok(None);
    };
    let file_name = requested_path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "staged user depfile has no file name",
        )
    })?;
    std::fs::create_dir_all(temp_root)?;
    let temp = tempfile::Builder::new()
        .prefix("staged-depfile-persist-")
        .tempdir_in(temp_root)?;
    let persisted_source = temp.path().join(file_name);
    std::fs::write(&persisted_source, bytes)?;
    *source_path = persisted_source.into();
    Ok(Some(temp))
}
