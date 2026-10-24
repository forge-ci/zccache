//! Initialized drop fields, excluding moved-out siblings and inactive variants.
use rustc_lint::LateContext;
use rustc_middle::{mir, ty};

pub(super) fn live_types<'tcx>(
    cx: &LateContext<'tcx>,
    body: &mir::Body<'tcx>,
    place: mir::Place<'tcx>,
    initialized: impl Fn(mir::Place<'tcx>) -> bool,
) -> Vec<ty::Ty<'tcx>> {
    let mut result = Vec::new();
    visit(
        cx,
        body,
        place.ty(&body.local_decls, cx.tcx).ty,
        place,
        &initialized,
        &mut result,
    );
    result
}

fn visit<'tcx>(
    cx: &LateContext<'tcx>,
    body: &mir::Body<'tcx>,
    value: ty::Ty<'tcx>,
    place: mir::Place<'tcx>,
    initialized: &impl Fn(mir::Place<'tcx>) -> bool,
    result: &mut Vec<ty::Ty<'tcx>>,
) {
    if !initialized(place) {
        return;
    }
    match value.kind() {
        ty::Adt(adt, args) => {
            if adt.destructor(cx.tcx).is_some()
                || super::guard_types::direct(cx, adt.did()).is_some()
            {
                result.push(value);
            } else if !adt.is_union()
                && !(cx.tcx.crate_name(adt.did().krate).as_str() == "core"
                    && cx.tcx.item_name(adt.did()).as_str() == "ManuallyDrop")
            {
                let known = super::drop_variants::known(cx, body, place);
                for (variant, definition) in adt.variants().iter_enumerated() {
                    if known.is_some_and(|known| known != variant) {
                        continue;
                    }
                    let base = if adt.is_enum() {
                        place
                            .project_deeper(&[mir::ProjectionElem::Downcast(None, variant)], cx.tcx)
                    } else {
                        place
                    };
                    for (index, field) in definition.fields.iter_enumerated() {
                        let field = field.ty(cx.tcx, args).skip_norm_wip();
                        visit(
                            cx,
                            body,
                            field,
                            base.project_deeper(
                                &[mir::ProjectionElem::Field(index, field)],
                                cx.tcx,
                            ),
                            initialized,
                            result,
                        );
                    }
                }
            }
        }
        ty::Tuple(fields) => visit_fields(cx, body, fields.iter(), place, initialized, result),
        ty::Closure(_, args) => visit_fields(
            cx,
            body,
            args.as_closure().upvar_tys().iter(),
            place,
            initialized,
            result,
        ),
        _ => result.push(value),
    }
}
fn visit_fields<'tcx>(
    cx: &LateContext<'tcx>,
    body: &mir::Body<'tcx>,
    fields: impl Iterator<Item = ty::Ty<'tcx>>,
    place: mir::Place<'tcx>,
    initialized: &impl Fn(mir::Place<'tcx>) -> bool,
    result: &mut Vec<ty::Ty<'tcx>>,
) {
    for (index, field) in fields.enumerate() {
        let place = place.project_deeper(
            &[mir::ProjectionElem::Field(
                rustc_abi::FieldIdx::from_usize(index),
                field,
            )],
            cx.tcx,
        );
        visit(cx, body, field, place, initialized, result);
    }
}
