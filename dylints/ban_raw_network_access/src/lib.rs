#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::def::Res;
use rustc_hir::def_id::DefId;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{FileName, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Rejects anything resolving into an HTTP transport crate outside the
    /// approved `zccache_download::http` module.
    ///
    /// ### Why is this bad?
    ///
    /// A raw client has no deadline by default, so a peer that accepts and then
    /// goes silent hangs the caller forever (#1968).
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// reqwest::Client::new().get(url).send().await?;
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// client.get(url, http::CONTROL).send(&cancel).await?;
    /// ```
    pub BAN_RAW_NETWORK_ACCESS,
    Deny,
    "confine HTTP transports to the budgeted zccache_download::http module"
}

const TRANSPORT_CRATES: &[&str] = &["reqwest", "hyper", "hyper_util", "ureq"];
const APPROVED_OWNER: &str = "crates/zccache-download/src/http/";

impl<'tcx> LateLintPass<'tcx> for BanRawNetworkAccess {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let def_id = match &expr.kind {
            ExprKind::MethodCall(..) => cx.typeck_results().type_dependent_def_id(expr.hir_id),
            ExprKind::Path(qpath) => match cx.qpath_res(qpath, expr.hir_id) {
                Res::Def(_, id) => Some(id),
                _ => None,
            },
            _ => None,
        };
        let Some(def_id) = def_id else {
            return;
        };
        if !(is_transport(cx, def_id) || instantiated_on_transport(cx, expr)) {
            return;
        }
        if source_filename(cx, expr).contains(APPROVED_OWNER) {
            return;
        }
        // Keep the substring "ui" out of this message: compiletest normalizes
        // the fixture directory name and would rewrite it to `$DIR` mid-word.
        cx.opt_span_lint(
            BAN_RAW_NETWORK_ACCESS,
            Some(expr.span),
            DiagDecorator(|diag| {
                diag.primary_message(
                    "raw HTTP transport use has no mandatory deadline; go through \
                     `zccache_download::http` (#1968)",
                );
            }),
        );
    }
}

/// True for an item defined in a transport crate. Definition paths ignore
/// re-exports, so `reqwest::header::ETAG` (defined in `http`) stays allowed. A
/// local module of that name counts as well, which is how the fixtures model a
/// crate they cannot depend on.
fn is_transport(cx: &LateContext<'_>, def_id: DefId) -> bool {
    let path = cx.get_def_path(def_id);
    let root = if def_id.is_local() {
        path.get(1)
    } else {
        path.first()
    };
    root.is_some_and(|name| TRANSPORT_CRATES.contains(&name.as_str()))
}

/// Catches a foreign trait method on a transport type, such as
/// `reqwest::Client::default()`, whose own definition lives in `core`.
fn instantiated_on_transport(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    let Some(args) = cx.typeck_results().node_args_opt(expr.hir_id) else {
        return false;
    };
    args.iter().flat_map(|arg| arg.walk()).any(|arg| {
        arg.as_type()
            .and_then(|ty| ty.ty_adt_def())
            .is_some_and(|adt| is_transport(cx, adt.did()))
    })
}

fn source_filename(cx: &LateContext<'_>, expr: &Expr<'_>) -> String {
    let name = match cx.sess().source_map().span_to_filename(expr.span) {
        FileName::Real(real) => real
            .path(RemapPathScopeComponents::DIAGNOSTICS)
            .to_string_lossy()
            .into_owned(),
        other => other
            .display(RemapPathScopeComponents::DIAGNOSTICS)
            .to_string(),
    };
    name.replace('\\', "/")
}

#[cfg(test)]
mod ui_test_support {
    include!("../../ui_test_support.rs");
}

#[test]
fn ui() {
    ui_test_support::run(env!("CARGO_PKG_NAME"), env!("CARGO_MANIFEST_DIR"));
}
