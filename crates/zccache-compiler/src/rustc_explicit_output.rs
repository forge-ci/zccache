//! Rustc's `-o` adaptation, shared by primary and secondary output discovery.

use crate::{RustcOutputPlan, RustcPathSyntax};

/// Resolve an implicit emit's destination when `-o` is present.
///
/// Explicit `kind=path` destinations must be resolved by the caller first.
/// `filename` is the normal, target-aware filename for this emit. Rustc adapts
/// `-o` only when more than one distinct emit lacks a kind-specific path.
#[must_use]
pub fn rustc_explicit_output_plan(
    output: &str,
    emit: &str,
    implicit_count: usize,
    filename: &str,
    suffix: &str,
    bin: bool,
    syntax: RustcPathSyntax,
) -> RustcOutputPlan {
    if implicit_count <= 1 {
        return RustcOutputPlan::Explicit(output.to_owned());
    }
    let filename = if emit == "metadata" || (emit == "link" && !bin) {
        filename.to_owned()
    } else {
        let stem = syntax.file_stem(output).unwrap_or(output);
        let extension = filename.rsplit_once('.').map_or("", |(_, ext)| ext);
        if extension.is_empty() {
            format!("{stem}{suffix}")
        } else {
            format!("{stem}{suffix}.{extension}")
        }
    };
    RustcOutputPlan::InDirectory {
        directory: syntax.parent(output).unwrap_or("").to_owned(),
        filename,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapted_o_preserves_suffixes_and_target_extensions() {
        for (emit, filename, bin, expected) in [
            (
                "metadata",
                "libprobe-xyz.rmeta",
                false,
                "libprobe-xyz.rmeta",
            ),
            ("obj", "probe-xyz.o", false, "custom-xyz.o"),
            ("dep-info", "probe-xyz.d", false, "custom-xyz.d"),
            ("link", "libprobe-xyz.rlib", false, "libprobe-xyz.rlib"),
            ("link", "probe-xyz", true, "custom-xyz"),
            ("link", "probe-xyz.exe", true, "custom-xyz.exe"),
        ] {
            for (output, directory, syntax) in [
                ("out/custom.output", "out", RustcPathSyntax::Unix),
                (r"C:\out\custom.output", r"C:\out", RustcPathSyntax::Windows),
            ] {
                assert_eq!(
                    rustc_explicit_output_plan(output, emit, 2, filename, "-xyz", bin, syntax),
                    RustcOutputPlan::InDirectory {
                        directory: directory.into(),
                        filename: expected.into()
                    }
                );
                assert_eq!(
                    rustc_explicit_output_plan(output, emit, 1, filename, "-xyz", bin, syntax),
                    RustcOutputPlan::Explicit(output.into())
                );
            }
        }
    }
}
