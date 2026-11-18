//! When a compiler argv is too large to pass to `exec` directly.
//!
//! The answer decides whether [`super::write_response_file_if_needed`] spills
//! the arguments into a `@file`. Spilling is needed on every platform, not
//! only Windows: on Unix cargo retries an oversized rustc invocation as
//! `rustc @argfile`, zccache expands that file to hash the arguments, and the
//! expanded argv must be spilled again or `exec` fails with `E2BIG` (#2210).

/// Windows `CreateProcess` accepts at most 32,767 UTF-16 units. The threshold
/// leaves room for the compiler path, quoting overhead and rounding.
const WINDOWS_MAX_CMDLINE_LEN: usize = 30_000;

/// Linux `MAX_ARG_STRLEN`: one argv string, including its NUL terminator, may
/// not exceed 32 pages (128 KiB with 4 KiB pages) or `execve` returns
/// `E2BIG`, whatever the total argv size.
const UNIX_MAX_ARG_STRLEN: usize = 128 * 1024;

/// Total argv budget on Unix. `ARG_MAX` covers argv plus the environment:
/// 1 MiB on macOS and 2 MiB on Linux with the default 8 MiB stack. A quarter
/// of the smaller one leaves the environment room to grow, and an argv this
/// large is rare enough that the extra temporary file costs nothing measurable.
const UNIX_MAX_ARGV_LEN: usize = 256 * 1024;

/// Whether `args` would exceed the host's process-creation limits if passed
/// directly. `windows` selects the host rules; production callers pass
/// `kernal_api::platform::host::target_is_windows()`.
pub(super) fn exceeds_exec_limit(args: &[String], windows: bool) -> bool {
    if windows {
        // Each argument may gain a separator and a pair of quotes.
        let estimated: usize = args.iter().map(|arg| arg.len() + 3).sum();
        return estimated >= WINDOWS_MAX_CMDLINE_LEN;
    }
    // Each argument costs its bytes, a NUL terminator and an argv pointer.
    let mut total = 0usize;
    for arg in args {
        if arg.len() + 1 > UNIX_MAX_ARG_STRLEN {
            return true;
        }
        total += arg.len() + 1 + std::mem::size_of::<usize>();
    }
    total >= UNIX_MAX_ARGV_LEN
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::CompilerFamily;

    fn rustc_args_with_check_cfg(values: usize) -> Vec<String> {
        // The issue #2210 build-script shape: a single `--check-cfg` value
        // list larger than `MAX_ARG_STRLEN`, which makes cargo use an argfile.
        let list: Vec<String> = (0..values).map(|i| format!("\"a {i}\"")).collect();
        vec![
            "--crate-name".to_owned(),
            "argfile_e2e".to_owned(),
            "--edition=2021".to_owned(),
            "src/lib.rs".to_owned(),
            "--check-cfg".to_owned(),
            format!("cfg(pad, values({}))", list.join(", ")),
            "--cfg".to_owned(),
            "pad=\"a 0\"".to_owned(),
        ]
    }

    #[test]
    fn unix_single_argument_over_max_arg_strlen_exceeds_limit() {
        let fits = vec!["x".repeat(UNIX_MAX_ARG_STRLEN - 1)];
        let too_long = vec!["x".repeat(UNIX_MAX_ARG_STRLEN)];
        assert!(!exceeds_exec_limit(&fits, false));
        assert!(exceeds_exec_limit(&too_long, false));
    }

    #[test]
    fn unix_total_argv_over_budget_exceeds_limit() {
        let many_small = vec!["--extern=dep=/target/debug/deps/libdep.rlib".to_owned(); 8_000];
        assert!(many_small.iter().all(|arg| arg.len() < 64));
        assert!(exceeds_exec_limit(&many_small, false));
    }

    #[test]
    fn unix_ordinary_compile_does_not_spill() {
        let args = rustc_args_with_check_cfg(10);
        assert!(!exceeds_exec_limit(&args, false));
        // A command line Windows would spill is still fine for Unix `exec`.
        let medium = vec!["y".repeat(40_000)];
        assert!(exceeds_exec_limit(&medium, true));
        assert!(!exceeds_exec_limit(&medium, false));
    }

    #[test]
    fn windows_threshold_is_unchanged() {
        assert!(!exceeds_exec_limit(&["z".repeat(29_990)], true));
        assert!(exceeds_exec_limit(&["z".repeat(30_000)], true));
    }

    /// Regression for #2210: the expanded argv of a cargo `rustc @argfile`
    /// invocation must be spilled into a rustc-format response file on every
    /// host. Before the fix this returned `None` off Windows and the daemon
    /// spawned rustc with the oversized argv, failing with `E2BIG`.
    #[test]
    fn oversized_rustc_argv_spills_to_rustc_response_file_on_every_host() {
        let args = rustc_args_with_check_cfg(20_000);
        assert!(args[5].len() > UNIX_MAX_ARG_STRLEN);
        let dir = tempfile::tempdir().unwrap();

        let spilled =
            super::super::write_response_file_if_needed(&args, dir.path(), CompilerFamily::Rustc)
                .unwrap()
                .expect("an argv too large for exec must be spilled to a response file");

        assert_eq!(spilled.at_arg(), format!("@{}", spilled.path.display()));
        let written = std::fs::read_to_string(&spilled.path).unwrap();
        let reparsed =
            super::super::parse_response_file_content_for(&written, CompilerFamily::Rustc);
        assert_eq!(reparsed, args);
    }

    #[test]
    fn small_rustc_argv_is_passed_directly() {
        let args = rustc_args_with_check_cfg(10);
        let dir = tempfile::tempdir().unwrap();
        let spilled =
            super::super::write_response_file_if_needed(&args, dir.path(), CompilerFamily::Rustc)
                .unwrap();
        assert!(spilled.is_none());
    }
}
