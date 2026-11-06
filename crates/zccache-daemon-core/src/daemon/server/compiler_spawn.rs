//! The one owner of compiler `SpawnSpec` construction in the daemon (#2027).
//!
//! Every compiler child the daemon runs is one of two kinds:
//!
//! - [`user_compile_spawn`]: the user's compile. Its argv passes through
//!   exactly as given (or as the response file that carries it).
//! - [`internal_scan_spawn`]: a run zccache makes for itself that changes the
//!   user's job type (`-M`, `-E`, `-fsyntax-only`, `-###`, include probes).
//!   Clang reports linker inputs as unused only because the job type changed,
//!   so that warning describes zccache's rewrite, not the user's code. The
//!   scan therefore appends the family's suppression flag last, and its
//!   callers must fail open: a failed scan disables the optimization, never
//!   the user's compile (see [`log_internal_scan_failure`]).
//!
//! `crates/zccache-daemon-core/clippy.toml` bans `kernal_api::SpawnSpec::new`
//! everywhere else in this crate, so a new compiler run has to pick one of
//! these constructors.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::compiler::response_file::TempResponseFile;
use crate::compiler::CompilerFamily;

/// Clang's suppression for warnings caused by a changed job type. GCC
/// accepts and ignores unknown `-Wno-*` flags, so the GNU family shares it.
pub(in crate::daemon) const UNUSED_ARGUMENT_SUPPRESSION: &str = "-Wno-unused-command-line-argument";

/// Spawn the user's compile with its argv unchanged.
#[allow(
    clippy::disallowed_methods,
    reason = "#2027: the compiler spawn module owns SpawnSpec construction"
)]
pub(in crate::daemon) fn user_compile_spawn(
    compiler: &Path,
    cwd: &Path,
    args: &[String],
    response_file: Option<&TempResponseFile>,
) -> kernal_api::SpawnSpec {
    let builder = kernal_api::SpawnSpec::new(compiler).current_dir(cwd);
    match response_file {
        Some(response_file) => builder.arg(response_file.at_arg()),
        None => builder.args(args),
    }
}

/// Spawn a compiler identity probe (`--version`, `-vV`, `-V`). It reads no
/// sources and its output is hashed, so its argv is exactly `flag`.
#[allow(
    clippy::disallowed_methods,
    reason = "#2027: the compiler spawn module owns SpawnSpec construction"
)]
pub(in crate::daemon) fn compiler_identity_probe_spawn(
    compiler: impl Into<std::ffi::OsString>,
    flag: &str,
) -> kernal_api::SpawnSpec {
    kernal_api::SpawnSpec::new(compiler).arg(flag)
}

/// The argv an internal scan runs: `scan_args` plus the family's
/// unused-argument suppression as the last option. Options must precede a
/// `--` separator (everything after it is an input), so the flag lands just
/// before it. Later `-W` flags win, so this also neutralizes a user's
/// `-Werror=unused-command-line-argument` for the scan only.
pub(in crate::daemon) fn scan_safe_args(
    family: CompilerFamily,
    scan_args: &[String],
) -> Vec<String> {
    let mut args = scan_args.to_vec();
    if matches!(family, CompilerFamily::Clang | CompilerFamily::Gcc) {
        let insertion = args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(args.len());
        args.insert(insertion, UNUSED_ARGUMENT_SUPPRESSION.to_owned());
    }
    // MSVC (cl.exe, clang-cl) has no unused-input diagnostic for these
    // probes, and Rust tools never run as scans: no flag for them.
    args
}

/// An internal scan's spawn spec plus the response file it reads, which must
/// outlive the child.
pub(in crate::daemon) struct InternalScanSpawn {
    spec: kernal_api::SpawnSpec,
    response_file: Option<TempResponseFile>,
}

impl InternalScanSpawn {
    /// The spec to run, and the response-file guard to hold until it exits.
    pub(in crate::daemon) fn into_parts(self) -> (kernal_api::SpawnSpec, Option<TempResponseFile>) {
        (self.spec, self.response_file)
    }
}

/// Spawn a zccache-internal compiler run. The suppression flag from
/// [`scan_safe_args`] is applied before any response file is written, so it
/// lands inside the response file when one is needed. Callers must treat
/// every failure as "skip the optimization" and log it through
/// [`log_internal_scan_failure`].
#[allow(
    clippy::disallowed_methods,
    reason = "#2027: the compiler spawn module owns SpawnSpec construction"
)]
pub(in crate::daemon) fn internal_scan_spawn(
    compiler: &Path,
    family: CompilerFamily,
    scan_args: &[String],
    cwd: Option<&Path>,
    response_file_dir: Option<&Path>,
) -> std::io::Result<InternalScanSpawn> {
    let args = scan_safe_args(family, scan_args);
    let response_file = match response_file_dir {
        Some(directory) => {
            crate::compiler::response_file::write_response_file_if_needed(&args, directory, family)?
        }
        None => None,
    };
    let mut spec = kernal_api::SpawnSpec::new(compiler);
    if let Some(cwd) = cwd {
        spec = spec.current_dir(cwd);
    }
    spec = match &response_file {
        Some(response_file) => spec.arg(response_file.at_arg()),
        None => spec.args(&args),
    };
    Ok(InternalScanSpawn {
        spec,
        response_file,
    })
}

static SCAN_FAILURE_LOGGED: AtomicBool = AtomicBool::new(false);

/// Record that an internal scan failed and the compile is falling back.
/// The first failure warns; repeats log at debug so a broken toolchain does
/// not flood the daemon log with one line per compile.
pub(in crate::daemon) fn log_internal_scan_failure(scan: &str, error: &dyn std::fmt::Display) {
    if SCAN_FAILURE_LOGGED.swap(true, Ordering::Relaxed) {
        tracing::debug!(scan, %error, "internal compiler scan failed; compiling without it");
    } else {
        tracing::warn!(
            scan,
            %error,
            "internal compiler scan failed; compiling without it (further failures log at debug)"
        );
    }
}

#[cfg(test)]
#[path = "compiler_spawn_tests.rs"]
mod tests;
