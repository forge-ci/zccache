#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

mod session_owner;

use rustc_errors::DiagDecorator;
use rustc_hir::def::Res;
use rustc_hir::{Expr, ExprKind, Item};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{symbol::Symbol, FileName, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// Require the bounded daemon process owner for child creation and waits.
    ///
    /// Resolved std/Tokio creation and native platform APIs are forbidden.
    /// Semantic kernal-api session operations belong in process_session.rs
    /// functions whose signatures require a direct finite budget. Ordinary
    /// callers use the process facade with a mandatory ProcessLiveness policy.
    /// Aliases, UFCS and function items are checked by their resolved DefId.
    ///
    /// This establishes capability placement and budget signatures, not a
    /// proof of termination. Runtime tests cover progress and cleanup.
    pub BAN_RAW_SUBPROCESS_IN_DAEMON,
    Deny,
    "ban raw Command::{spawn, output, status} in zccache-daemon production code"
}

/// Raw creation APIs remain forbidden even inside the semantic session owner.
const BANNED_METHOD_PATHS: &[&[&str]] = &[
    &["std", "process", "Command", "spawn"],
    &["std", "process", "Command", "output"],
    &["std", "process", "Command", "status"],
    &["tokio", "process", "Command", "spawn"],
    &["tokio", "process", "Command", "output"],
    &["tokio", "process", "Command", "status"],
];

const RAW_PROCESS_FUNCTIONS: &[&str] = &[
    "CreateProcessA",
    "CreateProcessW",
    "CreateProcessAsUserA",
    "CreateProcessAsUserW",
    "CreateProcessWithLogonW",
    "CreateProcessWithTokenW",
    "posix_spawn",
    "posix_spawnp",
    "fork",
    "vfork",
    "execv",
    "execve",
    "execvp",
    "execvpe",
    "execl",
    "execlp",
    "execle",
];

/// Only daemon production code is in scope. Dedicated fixture directories
/// remain free to launch test helpers.
const DAEMON_SOURCE_PREFIX: &str = "crates/zccache-daemon-core/src/daemon/";

impl<'tcx> LateLintPass<'tcx> for BanRawSubprocessInDaemon {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let filename = source_filename(cx, expr.span);
        let normalized = normalize_slashes(&filename);

        // Out-of-scope file → never fires.
        // UI fixtures live beside this lint rather than beneath the daemon
        // crate. Keeping that narrow exception lets the real UI test exercise
        // both the resolved-method matcher and the production scope guard.
        let is_ui_fixture = cx.sess().opts.unstable_opts.ui_testing
            || normalized.starts_with("ui/")
            || normalized.contains("/ui/");
        if !normalized.contains(DAEMON_SOURCE_PREFIX) && !is_ui_fixture {
            return;
        }

        if !is_ui_fixture
            && (cx.sess().is_test_crate()
                || normalized.contains("/tests/")
                || normalized.ends_with("_tests.rs"))
        {
            return;
        }

        match expr.kind {
            ExprKind::MethodCall(..) => {
                if let Some(def_id) = cx.typeck_results().type_dependent_def_id(expr.hir_id) {
                    check_resolved_path(cx, expr.span, def_id);
                }
            }
            // Associated functions can be invoked with UFCS or stored as
            // function items. Checking every resolved path blocks both forms;
            // matching only MethodCall/Call syntax leaves those trivial
            // mechanical bypasses open.
            ExprKind::Path(qpath) => {
                let Res::Def(_, def_id) = cx.qpath_res(&qpath, expr.hir_id) else {
                    return;
                };
                check_resolved_path(cx, expr.span, def_id);
            }
            _ => {}
        }
    }

    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        let filename = normalize_slashes(&source_filename(cx, item.span));
        let is_ui_fixture = cx.sess().opts.unstable_opts.ui_testing
            || filename.starts_with("ui/")
            || filename.contains("/ui/");
        if !filename.contains(DAEMON_SOURCE_PREFIX) && !is_ui_fixture {
            return;
        }
        if !is_ui_fixture
            && (cx.sess().is_test_crate()
                || filename.contains("/tests/")
                || filename.ends_with("_tests.rs"))
        {
            return;
        }
        let Ok(snippet) = cx.sess().source_map().span_to_snippet(item.span) else {
            return;
        };
        if !snippet.contains("extern ") {
            return;
        }
        for name in RAW_PROCESS_FUNCTIONS {
            if snippet.contains(&format!("fn {name}")) {
                emit_raw_platform(cx, item.span, name);
                return;
            }
        }
    }
}

fn check_resolved_path(
    cx: &LateContext<'_>,
    span: rustc_span::Span,
    def_id: rustc_hir::def_id::DefId,
) {
    if session_owner::check(cx, span, def_id) {
        return;
    }
    let path = cx.get_def_path(def_id);
    for banned in BANNED_METHOD_PATHS {
        if path_ends_with(&path, banned) {
            emit_lint(cx, span, banned);
            return;
        }
    }

    if path.last() == Some(&Symbol::intern("creation_flags")) {
        emit_message(
            cx,
            span,
            "`CommandExt::creation_flags` bypasses kernal-api; execute the configured command \
             through the daemon process helpers backed by `kernal_api::async_process`"
                .to_string(),
        );
        return;
    }

    let Some(name) = path.last().map(Symbol::as_str) else {
        return;
    };
    if RAW_PROCESS_FUNCTIONS.contains(&name) {
        emit_raw_platform(cx, span, &name);
    }
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span, banned: &[&str]) {
    emit_message(
        cx,
        span,
        format!(
            "`{}` bypasses kernal-api; execute the configured command through \
             the daemon process helpers backed by `kernal_api::async_process`",
            banned.join("::")
        ),
    );
}

fn emit_raw_platform(cx: &LateContext<'_>, span: rustc_span::Span, name: &str) {
    emit_message(
        cx,
        span,
        format!(
            "raw platform process API `{name}` bypasses kernal-api; remove the declaration or \
             call a daemon process helper backed by `kernal_api::async_process`"
        ),
    );
}

fn emit_message(cx: &LateContext<'_>, span: rustc_span::Span, message: String) {
    cx.opt_span_lint(
        BAN_RAW_SUBPROCESS_IN_DAEMON,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(message);
        }),
    );
}

fn source_filename(cx: &LateContext<'_>, span: rustc_span::Span) -> String {
    match cx.sess().source_map().span_to_filename(span) {
        FileName::Real(real_filename) => real_filename
            .local_path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                real_filename
                    .path(RemapPathScopeComponents::DIAGNOSTICS)
                    .to_string_lossy()
                    .into_owned()
            }),
        filename => filename
            .display(RemapPathScopeComponents::DIAGNOSTICS)
            .to_string(),
    }
}

fn normalize_slashes(path: &str) -> String {
    path.replace('\\', "/")
}

fn path_ends_with(path: &[Symbol], expected: &[&str]) -> bool {
    path.len() >= expected.len()
        && path[path.len() - expected.len()..]
            .iter()
            .zip(expected)
            .all(|(actual, expected)| *actual == Symbol::intern(expected))
}

#[cfg(test)]
mod ui_test_support {
    include!("../../ui_test_support.rs");
}

#[cfg(test)]
mod ui_dependencies;

#[test]
fn ui() {
    ui_test_support::run(env!("CARGO_PKG_NAME"), env!("CARGO_MANIFEST_DIR"));
    let _directory =
        ui_test_support::CurrentDirGuard::set(std::path::Path::new(env!("CARGO_MANIFEST_DIR")));
    dylint_testing::ui::Test::src_base(env!("CARGO_PKG_NAME"), "examples")
        .rustc_flags(ui_dependencies::dependency_flags())
        .run();
}
