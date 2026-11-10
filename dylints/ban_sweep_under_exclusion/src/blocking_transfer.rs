//! The canonical blocking launcher transfers a checked closure to one task.
//! Cancellation must release captured exclusion before opaque cleanup runs.
use super::guard_types::Scope;
use rustc_lint::LateContext;
use rustc_middle::{mir, ty};

pub(super) fn moves<'tcx>(
    cx: &LateContext<'tcx>,
    body: &mir::Body<'tcx>,
    terminator: &mir::Terminator<'tcx>,
    scope: Scope,
) -> Vec<mir::Place<'tcx>> {
    let mir::TerminatorKind::Call { func, args, .. } = &terminator.kind else {
        return Vec::new();
    };
    let ty::FnDef(definition, _) = *func.ty(&body.local_decls, cx.tcx).kind() else {
        return Vec::new();
    };
    if super::canonical_path(cx, definition) != "kernal_api::async_engine::launch_blocking" {
        return Vec::new();
    }
    let Some(argument) = args.first() else {
        return Vec::new();
    };
    let mir::Operand::Move(place) = argument.node else {
        return Vec::new();
    };
    let value = place.ty(&body.local_decls, cx.tcx).ty;
    let ty::Closure(definition, arguments) = *value.kind() else {
        return Vec::new();
    };
    if !definition.is_local()
        || !super::checks_guards(cx, cx.tcx.def_span(definition))
        || super::guard_types::borrowed(cx, value, scope, &mut Vec::new())
    {
        return Vec::new();
    }
    let output = arguments.as_closure().sig().skip_binder().output();
    if !super::checked_transfer::known_output(cx, output, &mut Vec::new())
        || super::guard_types::contains(cx, output, Scope::Ownership, &mut Vec::new())
    {
        return Vec::new();
    }
    let exclusion = if scope == Scope::Ownership {
        Scope::Any
    } else {
        scope
    };
    if !super::guard_types::contains(cx, value, scope, &mut Vec::new())
        || !cancellation_safe(cx, value, exclusion)
    {
        return Vec::new();
    }
    vec![place]
}

/// Checked closure captures use the same automatic drop-order proof as owners.
pub(super) fn cancellation_safe<'tcx>(
    cx: &LateContext<'tcx>,
    value: ty::Ty<'tcx>,
    scope: Scope,
) -> bool {
    matches!(value.kind(), ty::Closure(..)) && super::ordered_drop::safe(cx, value, scope)
}
