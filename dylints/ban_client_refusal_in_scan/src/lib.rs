#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{Expr, ExprKind, QPath};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{FileName, RemapPathScopeComponents, Span};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Inside the daemon's internal compiler scan modules, rejects building a
    /// client-visible refusal: `PchRefusal`, any `PchRefusalReason` variant,
    /// `Response::Error`, or `CompileExecResult::Error`.
    ///
    /// ### Why is this bad?
    ///
    /// This is the #1977 / #2027 bug class. The PCH include alias scan is a
    /// compiler run zccache makes for itself. It returned
    /// `PchRefusalReason::Unreadable` when that run failed, which failed a user
    /// compile that would have succeeded. A scan only enables an optimization.
    /// It reports a plain error, and only its fail-open caller decides what the
    /// client sees.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// return Err(PchRefusal { path, reason: PchRefusalReason::Unreadable(e) });
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// return Err(AliasScanError(e.to_string()));
    /// ```
    pub BAN_CLIENT_REFUSAL_IN_SCAN,
    Deny,
    "ban client-visible refusals inside internal compiler scan modules"
}

/// Modules that run zccache's own compiler scans (`internal_scan_spawn` and
/// its callers). Every entry is verified to exist by
/// `ci/check_dylint_wiring.py`.
const SCAN_SOURCE_PREFIXES: &[&str] = &[
    "crates/zccache-daemon-core/src/daemon/server/compiler_spawn.rs",
    "crates/zccache-daemon-core/src/daemon/server/handle_compile/pch_overlay_aliases.rs",
    "crates/zccache-daemon-core/src/daemon/server/handle_compile/pipeline/system_includes.rs",
];

impl<'tcx> LateLintPass<'tcx> for BanClientRefusalInScan {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let qpath: &QPath<'_> = match &expr.kind {
            ExprKind::Path(qpath) => qpath,
            ExprKind::Struct(qpath, ..) => qpath,
            _ => return,
        };
        if !in_scope(cx, expr.span) {
            return;
        }
        let Some(name) = refusal_name(cx, qpath, expr) else {
            return;
        };
        // Keep the substring "ui" out of this message: compiletest normalizes
        // the fixture directory name and would rewrite it to `$DIR` mid-word.
        let message = format!(
            "`{name}` is a client-visible refusal constructed inside an internal compiler scan; \
             return a plain scan error and let the fail-open caller decide (#2027)"
        );
        cx.opt_span_lint(
            BAN_CLIENT_REFUSAL_IN_SCAN,
            Some(expr.span),
            DiagDecorator(move |diag| {
                diag.primary_message(message.clone());
            }),
        );
    }
}

/// The banned item this path names, if any. Tuple-variant constructors
/// resolve to a `Ctor` whose parent is the variant itself.
fn refusal_name(cx: &LateContext<'_>, qpath: &QPath<'_>, expr: &Expr<'_>) -> Option<String> {
    let def_id = match cx.qpath_res(qpath, expr.hir_id) {
        Res::Def(DefKind::Ctor(..), id) => cx.tcx.parent(id),
        Res::Def(DefKind::Struct | DefKind::Variant, id) => id,
        _ => return None,
    };
    let path = cx.tcx.def_path_str(def_id);
    let banned = path.ends_with("PchRefusal")
        || path.contains("PchRefusalReason::")
        || path.ends_with("Response::Error")
        || path.ends_with("CompileExecResult::Error");
    banned.then_some(path)
}

fn in_scope(cx: &LateContext<'_>, span: Span) -> bool {
    let normalized = source_filename(cx, span).replace('\\', "/");
    // UI fixtures live beside this lint rather than beneath the daemon crate.
    let is_ui_fixture = normalized.starts_with("ui/") || normalized.contains("/ui/");
    is_ui_fixture
        || SCAN_SOURCE_PREFIXES
            .iter()
            .any(|prefix| normalized.contains(prefix))
}

fn source_filename(cx: &LateContext<'_>, span: Span) -> String {
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

#[cfg(test)]
mod ui_test_support {
    include!("../../ui_test_support.rs");
}

#[test]
fn ui() {
    ui_test_support::run(env!("CARGO_PKG_NAME"), env!("CARGO_MANIFEST_DIR"));
}
