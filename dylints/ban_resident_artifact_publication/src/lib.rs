#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;

use rustc_errors::DiagDecorator;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};

dylint_linting::declare_late_lint! {
    /// Resident live entries require an owner that demotes after persistence.
    pub BAN_RESIDENT_ARTIFACT_PUBLICATION,
    Deny,
    "confine resident artifact construction to the provisional publication owner"
}

fn is_cached_artifact_method(cx: &LateContext<'_>, id: DefId) -> bool {
    if cx.tcx.def_kind(id) != DefKind::AssocFn {
        return false;
    }
    cx.tcx.impl_of_assoc(id).is_some_and(|implementation| {
        cx.tcx
            .type_of(implementation)
            .instantiate_identity()
            .skip_norm_wip()
            .ty_adt_def()
            .is_some_and(|adt| cx.tcx.item_name(adt.did()).as_str() == "CachedArtifact")
    })
}

fn is_resident_constructor(cx: &LateContext<'_>, id: DefId) -> bool {
    is_cached_artifact_method(cx, id)
        && matches!(
            cx.tcx.item_name(id).as_str(),
            "from_artifact_data"
                | "from_artifact_data_with_modes"
                | "with_payloads"
                | "with_payloads_and_owner"
        )
}

impl<'tcx> LateLintPass<'tcx> for BanResidentArtifactPublication {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if cx.sess().opts.test {
            return;
        }
        let id = match &expr.kind {
            ExprKind::Path(path) => match cx.qpath_res(path, expr.hir_id) {
                Res::Def(_, id) => Some(id),
                _ => None,
            },
            ExprKind::MethodCall(..) => cx.typeck_results().type_dependent_def_id(expr.hir_id),
            _ => None,
        };
        let Some(id) = id else { return };
        if !is_resident_constructor(cx, id) {
            return;
        }
        let owner = cx.tcx.hir_enclosing_body_owner(expr.hir_id).to_def_id();
        if cx
            .tcx
            .def_path_str(owner)
            .contains("::cached_artifact::provisional::")
            || is_resident_constructor(cx, owner)
            || (is_cached_artifact_method(cx, owner)
                && cx.tcx.item_name(owner).as_str() == "from_provisional_staged")
        {
            return;
        }
        cx.opt_span_lint(
            BAN_RESIDENT_ARTIFACT_PUBLICATION,
            Some(expr.span),
            DiagDecorator(|diag| {
                diag.primary_message(
                    "resident artifact construction must go through ProvisionalArtifact (#1965)",
                );
            }),
        );
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
