//! Response-file grammar per compiler family.
//!
//! GCC, Clang and MSVC split an `@file` on whitespace, honour quotes and
//! escapes, and expand nested `@file`s ([`super::parse_response_file_content`]).
//! rustc does none of that: `rustc_driver_impl/src/args.rs` makes every line
//! one argument, verbatim, and never expands an `@` line read from a file.
//! Cargo relies on this when a command line is too long for Windows: its
//! argfile holds one argument per line, including `--check-cfg` values such
//! as `cfg(feature, values("a", "b"))` that the GCC grammar splits apart.

use std::collections::HashSet;
use std::path::Path;

use crate::CompilerFamily;

use super::ResponseFileError;

/// Read an argfile with the compiler family's native text encoding.
/// The Rust reader accepts UTF-8 only; MSVC response files may be UTF-16.
pub fn read_response_file_text_for(path: &Path, family: CompilerFamily) -> std::io::Result<String> {
    if family == CompilerFamily::Rustc {
        std::fs::read_to_string(path)
    } else {
        super::read_response_file_text(path)
    }
}

/// Split response-file `content` into arguments the way `family` reads it.
#[must_use]
pub fn parse_response_file_content_for(content: &str, family: CompilerFamily) -> Vec<String> {
    if family == CompilerFamily::Rustc {
        content.lines().map(str::to_owned).collect()
    } else {
        super::parse_response_file_content(content)
    }
}

/// Whether `family` expands `@file` arguments read from inside a response file.
#[must_use]
pub fn expands_nested_response_files(family: CompilerFamily) -> bool {
    family != CompilerFamily::Rustc
}

/// Like [`super::expand_response_files_in`], reading every `@file` the way
/// `family` does.
pub fn expand_response_files_in_for(
    args: &[String],
    base_dir: &Path,
    family: CompilerFamily,
) -> Result<Vec<String>, ResponseFileError> {
    let mut seen = HashSet::new();
    super::expand_recursive(args, base_dir, &mut seen, 0, family)
}
