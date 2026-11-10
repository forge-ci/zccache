//! Sealed index-reset transfer. Queue ownership is not a held publication lease.
//! No generic channel exemption: only this token and these exact phase owners.
use rustc_hir::{def::Res, def_id::DefId, Expr, ExprKind, Pat, PatKind};
use rustc_lint::{LateContext, LintContext};
use rustc_middle::ty;

const WAL: &str = "zccache_daemon_core::daemon::server::wal::";

pub(super) fn token(cx: &LateContext<'_>, definition: DefId) -> bool {
    cx.tcx.item_name(definition).as_str() == "IndexClearCommit"
        && super::canonical_path(cx, definition) == format!("{WAL}IndexClearCommit")
}

pub(super) fn queued<'tcx>(cx: &LateContext<'tcx>, value: ty::Ty<'tcx>) -> bool {
    let ty::Adt(definition, args) = value.kind() else {
        return false;
    };
    if cx.tcx.crate_name(definition.did().krate).as_str() != "kernal_api"
        || !matches!(
            cx.tcx.item_name(definition.did()).as_str(),
            "OneshotSender" | "OneshotReceiver"
        )
    {
        return false;
    }
    matches!(
        super::canonical_path(cx, definition.did()).as_str(),
        "kernal_api::async_engine::OneshotSender" | "kernal_api::async_engine::OneshotReceiver"
    ) && args
        .types()
        .any(|arg| matches!(arg.kind(), ty::Adt(def, _) if token(cx, def.did())))
}

pub(super) fn receive_owner(cx: &LateContext<'_>, definition: DefId) -> bool {
    let path = super::canonical_path(cx, definition);
    let expected = format!("{WAL}IndexClearReceiver::receive_for");
    path.strip_prefix(&expected)
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with("::{closure"))
}

fn role(cx: &LateContext<'_>, definition: DefId) -> Option<&'static str> {
    if !matches!(
        cx.tcx.item_name(definition).as_str(),
        "IndexClearCommit" | "IndexClearReceiver" | "ReservedIndexClear"
    ) {
        return None;
    }
    let path = super::canonical_path(cx, definition);
    match path.strip_prefix(WAL)? {
        "IndexClearCommit" => Some("token"),
        "IndexClearReceiver" => Some("receiver"),
        "ReservedIndexClear" => Some("sender"),
        _ => None,
    }
}

fn owned_by(cx: &LateContext<'_>, expected: &str) -> bool {
    let Some(owner) = super::enclosing_function(cx) else {
        return false;
    };
    let path = super::canonical_path(cx, owner);
    let expected = format!("{WAL}{expected}");
    path.strip_prefix(&expected)
        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with("::{closure"))
        && (cx.sess().opts.unstable_opts.ui_testing
            || super::filename(cx, cx.tcx.def_span(owner)).ends_with("/server/wal.rs"))
}

fn accessor(role: &str) -> &'static str {
    match role {
        "token" => "IndexClearCommit::into_publication",
        "receiver" => "IndexClearReceiver::receive_for",
        _ => "ReservedIndexClear::commit_for",
    }
}

fn reject(cx: &LateContext<'_>, span: rustc_span::Span) {
    super::emit(
        cx,
        span,
        "index Clear transfer belongs to its sealed phase owner",
    );
}

pub(super) fn check_expr<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
    if let ExprKind::Struct(..) = expr.kind {
        if let ty::Adt(definition, _) = cx.typeck_results().expr_ty(expr).kind() {
            if let Some(role) = role(cx, definition.did()) {
                let creator = if role == "token" {
                    "IndexClearCommit::new"
                } else {
                    "reserve_index_clear_blocking"
                };
                if !owned_by(cx, creator) {
                    reject(cx, expr.span);
                }
            }
        }
    }
    if let ExprKind::Field(receiver, _) = expr.kind {
        if let ty::Adt(definition, _) = cx.typeck_results().expr_ty(receiver).peel_refs().kind() {
            if let Some(role) = role(cx, definition.did()) {
                if !owned_by(cx, accessor(role)) {
                    reject(cx, expr.span);
                }
            }
        }
    }
    let definition = match expr.kind {
        ExprKind::Path(path) => match cx.qpath_res(&path, expr.hir_id) {
            Res::Def(_, definition) => definition,
            _ => return,
        },
        ExprKind::MethodCall(..) => match cx.typeck_results().type_dependent_def_id(expr.hir_id) {
            Some(definition) => definition,
            None => return,
        },
        _ => return,
    };
    if !matches!(
        cx.tcx.item_name(definition).as_str(),
        "new" | "into_publication" | "receive_for"
    ) {
        return;
    }
    let path = super::canonical_path(cx, definition);
    let owner = match path.strip_prefix(WAL) {
        Some("IndexClearCommit::new") => "ReservedIndexClear::commit_for",
        Some("IndexClearCommit::into_publication") => "IndexClearReceiver::receive_for",
        Some("IndexClearReceiver::receive_for") => "process_index_writer_command",
        _ => return,
    };
    let direct = matches!(expr.kind, ExprKind::MethodCall(..))
        || matches!(cx.tcx.parent_hir_node(expr.hir_id), rustc_hir::Node::Expr(Expr {
            kind: ExprKind::Call(callee, _), ..
        }) if callee.hir_id == expr.hir_id);
    if !direct || !owned_by(cx, owner) {
        reject(cx, expr.span);
    }
}

pub(super) fn check_pat<'tcx>(cx: &LateContext<'tcx>, pat: &'tcx Pat<'tcx>) {
    let PatKind::Struct(..) = pat.kind else {
        return;
    };
    if let ty::Adt(definition, _) = cx.typeck_results().node_type(pat.hir_id).peel_refs().kind() {
        if let Some(role) = role(cx, definition.did()) {
            if !owned_by(cx, accessor(role)) {
                reject(cx, pat.span);
            }
        }
    }
}
