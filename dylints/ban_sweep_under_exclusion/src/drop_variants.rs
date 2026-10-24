//! Prove a local enum's sole initialized variant without assuming mutable aliases.
use rustc_abi::VariantIdx;
use rustc_lint::LateContext;
use rustc_middle::mir;

pub(super) fn known<'tcx>(
    cx: &LateContext<'tcx>,
    body: &mir::Body<'tcx>,
    place: mir::Place<'tcx>,
) -> Option<VariantIdx> {
    resolve(cx, body, place, 0)
}

fn resolve<'tcx>(
    cx: &LateContext<'tcx>,
    body: &mir::Body<'tcx>,
    place: mir::Place<'tcx>,
    depth: usize,
) -> Option<VariantIdx> {
    if depth >= 32 {
        return None;
    }
    if body.args_iter().any(|argument| argument == place.local) {
        return None;
    }
    let mut definition = None;
    for block in body.basic_blocks.iter() {
        for statement in &block.statements {
            match &statement.kind {
                mir::StatementKind::Assign(assignment) => {
                    if overlaps(assignment.0, place) {
                        if definition.is_some()
                            || !place.projection.starts_with(assignment.0.projection)
                        {
                            return None;
                        }
                        definition = Some(&**assignment);
                    }
                    // Even a sole assignment does not prove the discriminant
                    // if a reference or raw pointer can mutate its storage.
                    match &assignment.1 {
                        mir::Rvalue::Ref(_, _, borrowed) | mir::Rvalue::RawPtr(_, borrowed)
                            if overlaps(*borrowed, place) =>
                        {
                            return None
                        }
                        _ => {}
                    }
                }
                mir::StatementKind::SetDiscriminant { place: changed, .. }
                    if overlaps(**changed, place) =>
                {
                    return None
                }
                _ => {}
            }
        }
        match &block.terminator().kind {
            mir::TerminatorKind::Call { destination, .. } if overlaps(*destination, place) => {
                return None
            }
            mir::TerminatorKind::InlineAsm { .. } => return None,
            _ => {}
        }
    }
    let (target, value) = definition?;
    from_value(
        cx,
        body,
        value,
        &place.projection[target.projection.len()..],
        depth + 1,
    )
}

fn from_value<'tcx>(
    cx: &LateContext<'tcx>,
    body: &mir::Body<'tcx>,
    value: &mir::Rvalue<'tcx>,
    projection: &[mir::PlaceElem<'tcx>],
    depth: usize,
) -> Option<VariantIdx> {
    match value {
        mir::Rvalue::Use(operand, _) => {
            let source = operand.place()?.project_deeper(projection, cx.tcx);
            resolve(cx, body, source, depth)
        }
        mir::Rvalue::Aggregate(kind, operands) => {
            if projection.is_empty() {
                return match **kind {
                    mir::AggregateKind::Adt(_, variant, _, _, None) => Some(variant),
                    _ => None,
                };
            }
            let mut remaining = projection;
            if let Some(mir::ProjectionElem::Downcast(_, wanted)) = remaining.first() {
                let mir::AggregateKind::Adt(_, variant, _, _, None) = **kind else {
                    return None;
                };
                if variant != *wanted {
                    return None;
                }
                remaining = &remaining[1..];
            }
            if !matches!(
                **kind,
                mir::AggregateKind::Adt(_, _, _, _, None)
                    | mir::AggregateKind::Tuple
                    | mir::AggregateKind::Closure(..)
            ) {
                return None;
            }
            let Some(mir::ProjectionElem::Field(field, _)) = remaining.first() else {
                return None;
            };
            let source = operands
                .get(*field)?
                .place()?
                .project_deeper(&remaining[1..], cx.tcx);
            resolve(cx, body, source, depth)
        }
        _ => None,
    }
}
fn overlaps<'tcx>(left: mir::Place<'tcx>, right: mir::Place<'tcx>) -> bool {
    left.local == right.local
        && (left.projection.starts_with(right.projection)
            || right.projection.starts_with(left.projection))
}
