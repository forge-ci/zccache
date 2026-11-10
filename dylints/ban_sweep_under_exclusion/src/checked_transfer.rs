//! A checked synchronous callee can take explicit ownership from its caller.
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
    let ty::FnDef(definition, arguments) = *func.ty(&body.local_decls, cx.tcx).kind() else {
        return Vec::new();
    };
    let Some(local) = definition.as_local() else {
        return super::blocking_transfer::moves(cx, body, terminator, scope);
    };
    if !matches!(
        cx.tcx.def_kind(definition),
        rustc_hir::def::DefKind::Fn | rustc_hir::def::DefKind::AssocFn
    ) || cx.tcx.hir_maybe_body_owned_by(local).is_none()
        || cx.tcx.asyncness(definition).is_async()
        || !super::checks_guards(cx, cx.tcx.def_span(definition))
    {
        return Vec::new();
    }
    let declared = cx
        .tcx
        .fn_sig(definition)
        .instantiate_identity()
        .skip_norm_wip()
        .skip_binder();
    let actual = cx
        .tcx
        .fn_sig(definition)
        .instantiate(cx.tcx, arguments)
        .skip_norm_wip()
        .skip_binder();
    // Returning guard ownership requires a separate proof at the destination.
    // Do not treat opaque/generic outputs as an implicit release contract.
    if !known_output(cx, actual.output(), &mut Vec::new())
        || super::guard_types::contains(cx, actual.output(), Scope::Ownership, &mut Vec::new())
    {
        return Vec::new();
    }
    declared
        .inputs()
        .iter()
        .zip(args)
        .filter_map(|(formal, argument)| {
            let mir::Operand::Move(place) = argument.node else {
                return None;
            };
            // An instantiated generic T is insufficient: the callee itself must
            // see and check the guard, including unwind paths. References leave
            // ownership at the caller even if their wrapper is moved.
            (super::guard_types::contains(cx, *formal, scope, &mut Vec::new())
                && !super::guard_types::borrowed(cx, *formal, scope, &mut Vec::new()))
            .then_some(place)
        })
        .collect()
}

pub(super) fn known_output<'tcx>(
    cx: &LateContext<'tcx>,
    value: ty::Ty<'tcx>,
    seen: &mut Vec<ty::Ty<'tcx>>,
) -> bool {
    if seen.contains(&value) {
        return true;
    }
    seen.push(value);
    match value.kind() {
        ty::Alias(..) | ty::Param(..) | ty::Coroutine(..) | ty::Dynamic(..) => false,
        ty::Tuple(fields) => fields.iter().all(|field| known_output(cx, field, seen)),
        ty::Array(field, _) | ty::Slice(field) | ty::Ref(_, field, _) | ty::RawPtr(field, _) => {
            known_output(cx, *field, seen)
        }
        ty::Adt(definition, arguments) if definition.did().is_local() => definition
            .all_fields()
            .all(|field| known_output(cx, field.ty(cx.tcx, arguments).skip_norm_wip(), seen)),
        ty::Adt(_, arguments) => arguments.types().all(|field| known_output(cx, field, seen)),
        _ => true,
    }
}
