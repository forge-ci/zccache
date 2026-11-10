//! Single-file destination rollback has authority separate from cache deletion.
use rustc_hir::{def::Res, def_id::DefId, Expr, ExprKind, Pat, PatKind};
use rustc_lint::LateContext;
use rustc_middle::ty;
const PREFIX: &str = "zccache_daemon_core::daemon::server::persist::output_destination::";

pub(super) fn unlink(cx: &LateContext<'_>, definition: DefId) -> bool {
    matches!(
        cx.tcx.def_kind(definition),
        rustc_hir::def::DefKind::Fn | rustc_hir::def::DefKind::AssocFn
    ) && cx.tcx.item_name(definition).as_str() == "unlink"
        && super::canonical_path(cx, definition) == format!("{PREFIX}OutputDestination::unlink")
}
pub(super) fn authority(cx: &LateContext<'_>, definition: DefId) -> bool {
    role(cx, definition).is_some()
}

fn role(cx: &LateContext<'_>, definition: DefId) -> Option<&'static str> {
    match cx.tcx.item_name(definition).as_str() {
        "OutputDestination" | "OutputScope" | "PrivateOutputNamespace" => {}
        _ => return None,
    }
    match super::canonical_path(cx, definition).strip_prefix(PREFIX)? {
        "OutputDestination" => Some("output"),
        "OutputScope" => Some("scope"),
        "PrivateOutputNamespace" => Some("private"),
        _ => None,
    }
}
fn owned_by(cx: &LateContext<'_>, names: &[&str]) -> bool {
    super::enclosing_function(cx).is_some_and(|owner| {
        let path = super::canonical_path(cx, owner);
        names.iter().any(|name| path == format!("{PREFIX}{name}"))
    })
}
fn path_view(cx: &LateContext<'_>) -> bool {
    let Some(owner) = super::enclosing_function(cx) else {
        return false;
    };
    if cx.tcx.item_name(owner).as_str() != "as_ref" {
        return false;
    }
    let Some(implementation) = cx.tcx.impl_of_assoc(owner) else {
        return false;
    };
    if !cx.tcx.impl_is_of_trait(implementation) {
        return false;
    }
    let trait_id = cx.tcx.impl_trait_ref(implementation).skip_binder().def_id;
    if super::canonical_path(cx, trait_id) != "core::convert::AsRef" {
        return false;
    }
    let self_ty = cx
        .tcx
        .type_of(implementation)
        .instantiate_identity()
        .skip_norm_wip();
    matches!(self_ty.kind(), ty::Adt(definition, _) if role(cx, definition.did()) == Some("output"))
}
fn reject(cx: &LateContext<'_>, span: rustc_span::Span) {
    super::emit(
        cx,
        span,
        "output mutation requires its sealed namespace authority",
    );
}
pub(super) fn check_expr(cx: &LateContext<'_>, expr: &Expr<'_>) {
    if let ExprKind::Struct(..) = expr.kind {
        if let ty::Adt(definition, _) = cx.typeck_results().expr_ty(expr).kind() {
            let owners: &[&str] = match role(cx, definition.did()) {
                Some("output") => &[
                    "OutputScope::output",
                    "PrivateOutputNamespace::output",
                    "OutputDestination::sibling_temporary",
                ],
                Some("scope") => &["OutputScope::for_state"],
                Some("private") => &["PrivateOutputNamespace::create"],
                _ => &[],
            };
            if !owners.is_empty() && !owned_by(cx, owners) {
                reject(cx, expr.span);
            }
        }
    }
    if let ExprKind::Field(value, _) = expr.kind {
        if let ty::Adt(definition, _) = cx.typeck_results().expr_ty(value).peel_refs().kind() {
            let field_role = role(cx, definition.did());
            let owners: &[&str] = match field_role {
                Some("output") => &[
                    "OutputDestination::unlink",
                    "OutputDestination::sibling_temporary",
                ],
                Some("scope") => &["OutputScope::output"],
                Some("private") => &["PrivateOutputNamespace::output"],
                _ => &[],
            };
            if !owners.is_empty()
                && !owned_by(cx, owners)
                && !(field_role == Some("output") && path_view(cx))
            {
                reject(cx, expr.span);
            }
        }
    }
    let definition = match expr.kind {
        ExprKind::Path(path) => match cx.qpath_res(&path, expr.hir_id) {
            Res::Def(_, definition) => definition,
            _ => return,
        },
        _ => return,
    };
    if super::canonical_path(cx, definition) == format!("{PREFIX}PrivateOutputNamespace::create") {
        let direct = matches!(cx.tcx.parent_hir_node(expr.hir_id), rustc_hir::Node::Expr(Expr { kind: ExprKind::Call(callee, _), .. }) if callee.hir_id == expr.hir_id);
        let owner = super::enclosing_function(cx).is_some_and(|owner| super::canonical_path(cx, owner) == "zccache_daemon_core::daemon::server::persist::staged_store::publish::persist_staged_generation");
        if !direct || !owner {
            reject(cx, expr.span);
        }
    }
}

fn valid_unlink(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    let ExprKind::Call(callee, args) = expr.kind else {
        return false;
    };
    let ExprKind::Path(path) = callee.kind else {
        return false;
    };
    let Res::Def(_, def) = cx.qpath_res(&path, callee.hir_id) else {
        return false;
    };
    if super::canonical_path(cx, def) != "std::fs::remove_file" {
        return false;
    }
    matches!(args, [argument] if matches!(argument.kind, ExprKind::AddrOf(_, _, field)
        if matches!(field.kind, ExprKind::Field(value, name)
            if name.name.as_str() == "path"
                && matches!(value.kind, ExprKind::Path(path) if matches!(cx.qpath_res(&path, value.hir_id), Res::Local(_)))
                && matches!(cx.typeck_results().expr_ty(value).peel_refs().kind(), ty::Adt(def, _) if role(cx, def.did()) == Some("output")))))
}

pub(super) fn check_fn(cx: &LateContext<'_>, body: &rustc_hir::Body<'_>, owner: DefId) {
    if !unlink(cx, owner) {
        return;
    }
    let valid = body.params.len() == 1
        && matches!(body.value.kind, ExprKind::Block(block, _)
        if block.stmts.is_empty() && block.expr.is_some_and(|expr| valid_unlink(cx, expr)));
    if !valid {
        reject(cx, body.value.span);
    }
}

pub(super) fn check_pat(cx: &LateContext<'_>, pat: &Pat<'_>) {
    if !matches!(pat.kind, PatKind::Struct(..)) {
        return;
    }
    if let ty::Adt(definition, _) = cx.typeck_results().node_type(pat.hir_id).peel_refs().kind() {
        if role(cx, definition.did()).is_some() {
            reject(cx, pat.span);
        }
    }
}
