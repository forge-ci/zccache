//! Automatic field destruction can release owned exclusion before cleanup.
use super::guard_types::Scope;
use rustc_lint::LateContext;
use rustc_middle::{mir, ty};

/// The resolved language drop function and MIR Drop both destroy an owned
/// value. A borrowed value or unknown function pointer is not a release.
pub(super) fn owned_place<'tcx>(
    cx: &LateContext<'tcx>,
    body: &mir::Body<'tcx>,
    terminator: &mir::Terminator<'tcx>,
) -> Option<mir::Place<'tcx>> {
    match &terminator.kind {
        mir::TerminatorKind::Drop { place, .. } => Some(*place),
        mir::TerminatorKind::Call { func, args, .. } => {
            let ty::FnDef(id, _) = *func.ty(&body.local_decls, cx.tcx).kind() else {
                return None;
            };
            if super::canonical_path(cx, id) != "core::mem::drop" {
                return None;
            }
            let mir::Operand::Move(place) = args.first()?.node else {
                return None;
            };
            Some(place)
        }
        _ => None,
    }
}

pub(super) fn safe<'tcx>(cx: &LateContext<'tcx>, value: ty::Ty<'tcx>, scope: Scope) -> bool {
    check(cx, value, scope, 0)
}
fn structural<'tcx>(cx: &LateContext<'tcx>, definition: ty::AdtDef<'tcx>) -> bool {
    !definition.is_union()
        && definition.destructor(cx.tcx).is_none()
        && (definition.did().is_local()
            || matches!(
                super::canonical_path(cx, definition.did()).as_str(),
                "core::option::Option" | "core::result::Result"
            ))
}
fn check<'tcx>(cx: &LateContext<'tcx>, value: ty::Ty<'tcx>, scope: Scope, depth: usize) -> bool {
    if depth >= 32 {
        return false;
    }
    match value.kind() {
        ty::Adt(definition, _) if super::guard_types::direct(cx, definition.did()).is_some() => {
            true
        }
        ty::Closure(_, arguments) => fields(
            cx,
            arguments.as_closure().upvar_tys().iter().collect(),
            scope,
            depth,
        ),
        ty::Tuple(values) => fields(cx, values.iter().collect(), scope, depth),
        ty::Adt(definition, arguments) if structural(cx, *definition) => {
            definition.variants().iter().all(|variant| {
                fields(
                    cx,
                    variant
                        .fields
                        .iter()
                        .map(|field| field.ty(cx.tcx, arguments).skip_norm_wip())
                        .collect(),
                    scope,
                    depth,
                )
            })
        }
        _ => false,
    }
}
fn owns<'tcx>(cx: &LateContext<'tcx>, value: ty::Ty<'tcx>, scope: Scope) -> bool {
    super::guard_types::contains(cx, value, scope, &mut Vec::new())
}
fn fields<'tcx>(
    cx: &LateContext<'tcx>,
    values: Vec<ty::Ty<'tcx>>,
    scope: Scope,
    depth: usize,
) -> bool {
    let mut held = values
        .iter()
        .filter(|field| owns(cx, **field, scope))
        .count();
    for field in values {
        let own = owns(cx, field, scope);
        // A nested owner can order its own releases, but it cannot release a
        // guard owned by a different field or a different caller variable.
        if held != 0 && !(own && held == 1 && check(cx, field, scope, depth + 1)) {
            let effects = super::effects::drop_value(cx, field, &mut Vec::new(), &mut Vec::new());
            let forbidden = super::effects::SWEEP
                | super::effects::UNKNOWN
                | super::effects::ESCAPE
                | if scope == Scope::Critical {
                    super::effects::BULK
                } else {
                    0
                };
            if effects & forbidden != 0 {
                return false;
            }
        }
        if own {
            if !releases(cx, field, scope, depth + 1) {
                return false;
            }
            held -= 1;
        }
    }
    true
}
fn releases<'tcx>(cx: &LateContext<'tcx>, value: ty::Ty<'tcx>, scope: Scope, depth: usize) -> bool {
    if depth >= 32 {
        return false;
    }
    if !owns(cx, value, scope) {
        return true;
    }
    match value.kind() {
        ty::Adt(definition, _) if super::guard_types::direct(cx, definition.did()).is_some() => {
            true
        }
        ty::Tuple(fields) => fields
            .iter()
            .all(|field| releases(cx, field, scope, depth + 1)),
        ty::Closure(_, arguments) => arguments
            .as_closure()
            .upvar_tys()
            .iter()
            .all(|field| releases(cx, field, scope, depth + 1)),
        ty::Adt(definition, arguments) if structural(cx, *definition) => {
            definition.all_fields().all(|field| {
                releases(
                    cx,
                    field.ty(cx.tcx, arguments).skip_norm_wip(),
                    scope,
                    depth + 1,
                )
            })
        }
        // Shared and manually suppressed wrappers do not prove release.
        _ => false,
    }
}
