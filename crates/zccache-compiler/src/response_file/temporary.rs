//! Temporary response-file creation shared by compiler argument encoders.

use std::path::Path;

use zccache_core::NormalizedPath;

use super::TempResponseFile;

/// Rewritten compiler argv and the files that protect literal `@` arguments.
pub struct RustcLiteralResponseFiles {
    arguments: Vec<String>,
    // Each argument file must outlive the compiler process.
    _files: Vec<TempResponseFile>,
}

impl RustcLiteralResponseFiles {
    /// Arguments to pass directly to the compiler.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.arguments
    }
}

/// Preserve literal `@` arguments obtained from a successful Rust argfile
/// expansion. Each is carried in its own one-argument file, so other argv
/// values (including newlines) retain their original order and bytes.
pub fn preserve_rustc_literal_at_args(
    args: &[String],
    tmp_dir: &Path,
) -> std::io::Result<Option<RustcLiteralResponseFiles>> {
    if !args.iter().any(|arg| arg.starts_with('@')) {
        return Ok(None);
    }
    let mut arguments = Vec::with_capacity(args.len());
    let mut files = Vec::new();
    for arg in args {
        if arg.starts_with('@') {
            if arg.contains('\n') {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "expanded literal @ argument contains a newline",
                ));
            }
            // No trailing newline: a literal trailing CR must survive too.
            let file = write_temp_response_file(arg, tmp_dir)?;
            arguments.push(file.at_arg());
            files.push(file);
        } else {
            // Cache-key arguments and the rewritten execution argv coexist.
            arguments.push(arg.clone());
        }
    }
    Ok(Some(RustcLiteralResponseFiles {
        arguments,
        _files: files,
    }))
}

pub(super) fn write_temp_response_file(
    content: &str,
    tmp_dir: &Path,
) -> std::io::Result<TempResponseFile> {
    let id = super::RSP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path =
        NormalizedPath::new(tmp_dir.join(format!("zccache_{}_{}.rsp", std::process::id(), id)));
    let file = TempResponseFile { path };
    std::fs::write(&file.path, content)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn literal_arguments_roundtrip_without_changing_other_values() {
        let dir = tempfile::tempdir().unwrap();
        let args = ["@first\r", "value\nwith newline", "@", "@second"].map(str::to_owned);
        let guard = preserve_rustc_literal_at_args(&args, dir.path())
            .unwrap()
            .unwrap();
        let expanded = crate::response_file::expand_response_files_in_for(
            guard.args(),
            dir.path(),
            crate::CompilerFamily::Rustc,
        )
        .unwrap();
        assert_eq!(expanded, args);
        drop(guard);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
