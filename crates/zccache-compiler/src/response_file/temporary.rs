//! Temporary response-file creation shared by compiler argument encoders.

use std::path::Path;

use zccache_core::NormalizedPath;

use super::TempResponseFile;

pub(super) fn write_temp_response_file(
    content: &str,
    tmp_dir: &Path,
) -> std::io::Result<TempResponseFile> {
    let id = super::RSP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path =
        NormalizedPath::new(tmp_dir.join(format!("zccache_{}_{}.rsp", std::process::id(), id)));
    std::fs::write(&path, content)?;
    Ok(TempResponseFile { path })
}
