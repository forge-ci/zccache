//! Optional CWD/argv diagnostic for wrapper invocations.
//!
//! Gated behind `ZCCACHE_DIAG_CWD=1` so it costs nothing in normal operation.
//! Emits a single tab-separated line to stderr at the very start of every
//! wrapper invocation, before any CWD mutation. Used to diagnose cases
//! (issue #683) where the wrapper appears to capture an unexpected CWD before
//! sending the compile request to the daemon — typically because an outer
//! shim/build system has chdir'd before exec'ing `zccache.exe`.

use std::io::Write;
use std::path::PathBuf;

const LINE_TAG: &str = "ZCCACHE_DIAG_CWD";

/// Emit the diagnostic to stderr if `ZCCACHE_DIAG_CWD` is `1` or `true`.
/// No-op otherwise. Errors writing to stderr are intentionally
/// ignored — diagnostics must never affect the wrapper's exit status.
pub(super) fn emit(args: &[String]) {
    if !crate::core::config::diag_cwd_enabled() {
        return;
    }
    let _ = emit_to(&mut std::io::stderr().lock(), args);
}

fn emit_to(writer: &mut dyn Write, args: &[String]) -> std::io::Result<()> {
    let pid = std::process::id();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("<unavailable>"));
    let tmp = std::env::temp_dir();
    let argv0 = std::env::args_os().next().unwrap_or_default();
    let argv0_display = std::path::Path::new(&argv0).display().to_string();

    writeln!(
        writer,
        "{LINE_TAG}\tpid={pid}\tcwd={cwd}\ttmp={tmp}\targv0={argv0}\targs={args:?}",
        cwd = cwd.display(),
        tmp = tmp.display(),
        argv0 = argv0_display,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_single_tagged_line() {
        let mut buf = Vec::new();
        let args = vec!["clang++".to_string(), "-c".to_string(), "a.cpp".to_string()];
        emit_to(&mut buf, &args).unwrap();
        let line = String::from_utf8(buf).unwrap();

        assert!(line.starts_with(LINE_TAG), "should start with tag: {line}");
        assert_eq!(line.matches('\n').count(), 1, "should be one line: {line}");
        assert!(line.contains("\tpid="));
        assert!(line.contains("\tcwd="));
        assert!(line.contains("\ttmp="));
        assert!(line.contains("\targv0="));
        assert!(line.contains("\targs="));
    }

    #[test]
    fn args_round_trip_through_line() {
        let mut buf = Vec::new();
        let args = vec![
            "clang++".to_string(),
            "-c".to_string(),
            "weird path/foo.cpp".to_string(),
        ];
        emit_to(&mut buf, &args).unwrap();
        let line = String::from_utf8(buf).unwrap();

        assert!(line.contains("clang++"));
        assert!(line.contains("weird path/foo.cpp"));
    }
}
