#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;

use rustc_errors::DiagDecorator;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{Expr, ExprKind, HirId, Node};
use rustc_lint::{LateContext, LateLintPass, LintContext};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Rejects `std::process::Stdio::null` and `StreamMode::Null` unless the
    /// value goes straight to a child's stdin.
    ///
    /// ### Why is this bad?
    ///
    /// A helper's stdout and stderr are the evidence needed to diagnose a
    /// stalled or failing helper (#1970).
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// command.stderr(Stdio::null());
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// zccache_core::small_tool::output_for_recorded("tool", spec, bound)?;
    /// ```
    pub BAN_SWALLOWED_CHILD_STDIO,
    Deny,
    "ban null sinks for child stdout and stderr"
}

impl<'tcx> LateLintPass<'tcx> for BanSwallowedChildStdio {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let ExprKind::Path(qpath) = &expr.kind else {
            return;
        };
        let Some(name) = null_sink_name(cx, cx.qpath_res(qpath, expr.hir_id)) else {
            return;
        };
        if flows_to_stdin(cx, expr.hir_id) {
            return;
        }
        // Keep the substring "ui" out of this message: compiletest normalizes
        // the fixture directory name and would rewrite it to `$DIR` mid-word.
        let message = format!(
            "`{name}` discards a child's output stream; capture it with \
             `zccache_core::small_tool`, or allow it with a stated reason for a \
             detached or existence-only child (#1970)"
        );
        cx.opt_span_lint(
            BAN_SWALLOWED_CHILD_STDIO,
            Some(expr.span),
            DiagDecorator(move |diag| {
                diag.primary_message(message.clone());
            }),
        );
    }
}

/// The null sink this resolved path names, if any. Unit-variant values resolve
/// to a `Ctor` whose parent is the variant.
fn null_sink_name(cx: &LateContext<'_>, res: Res) -> Option<String> {
    let def_id = match res {
        Res::Def(DefKind::Ctor(..), id) => cx.tcx.parent(id),
        Res::Def(DefKind::Variant | DefKind::AssocFn, id) => id,
        _ => return None,
    };
    let path = cx.tcx.def_path_str(def_id);
    (path.ends_with("Stdio::null") || path.ends_with("StreamMode::Null")).then_some(path)
}

/// Whether the value reaches a `stdin` call or field directly, passing only
/// through its own call and through `if`/`match`/block value positions.
fn flows_to_stdin(cx: &LateContext<'_>, start: HirId) -> bool {
    let mut child = start;
    for (id, node) in cx.tcx.hir_parent_iter(start) {
        match node {
            Node::Block(_) | Node::Arm(_) => {}
            Node::ExprField(field) => return field.ident.name.as_str() == "stdin",
            Node::Expr(parent) => match &parent.kind {
                ExprKind::If(..) | ExprKind::Match(..) | ExprKind::Block(..) => {}
                ExprKind::Call(callee, _) if callee.hir_id == child => {}
                ExprKind::Call(callee, _) => return names_stdin(cx, callee),
                ExprKind::MethodCall(segment, receiver, ..) => {
                    return receiver.hir_id != child && segment.ident.name.as_str() == "stdin";
                }
                _ => return false,
            },
            _ => return false,
        }
        child = id;
    }
    false
}

fn names_stdin(cx: &LateContext<'_>, callee: &Expr<'_>) -> bool {
    let ExprKind::Path(qpath) = &callee.kind else {
        return false;
    };
    match cx.qpath_res(qpath, callee.hir_id) {
        Res::Def(DefKind::AssocFn | DefKind::Fn, id) => {
            cx.tcx.def_path_str(id).ends_with("::stdin")
        }
        _ => false,
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
