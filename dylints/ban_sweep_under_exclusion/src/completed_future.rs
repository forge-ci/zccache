//! Must-completion proof for a compiler-generated coroutine's direct drop.
//! Custom Future implementations retain their normal destructor effects.
use rustc_lint::LateContext;
use rustc_middle::{mir, ty};

pub(super) fn at_drop<'tcx>(
    cx: &LateContext<'tcx>,
    body: &mir::Body<'tcx>,
    dropping: mir::BasicBlock,
    place: mir::Place<'tcx>,
) -> bool {
    let mut value = place.ty(&body.local_decls, cx.tcx).ty;
    if matches!(value.kind(), ty::Alias(..)) {
        value = cx
            .tcx
            .try_normalize_erasing_regions(cx.typing_env(), ty::Unnormalized::new_wip(value))
            .unwrap_or(value);
    }
    if let ty::Alias(alias) = value.kind() {
        if matches!(alias.kind, ty::Opaque { .. }) {
            value = cx
                .tcx
                .type_of(alias.kind.def_id())
                .instantiate(cx.tcx, alias.args)
                .skip_norm_wip();
        }
    }
    if !matches!(value.kind(), ty::Coroutine(..)) {
        return false;
    }
    let mut ready_edges = Vec::new();
    for (block, data) in body.basic_blocks.iter_enumerated() {
        let mir::TerminatorKind::Call {
            func,
            args,
            destination,
            target: Some(target),
            ..
        } = &data.terminator().kind
        else {
            continue;
        };
        let ty::FnDef(id, _) = *func.ty(&body.local_decls, cx.tcx).kind() else {
            continue;
        };
        if cx.tcx.item_name(id).as_str() != "poll"
            || cx.tcx.def_kind(id) != rustc_hir::def::DefKind::AssocFn
        {
            continue;
        }
        let Some(trait_id) = cx.tcx.associated_item(id).trait_container(cx.tcx) else {
            continue;
        };
        if super::canonical_path(cx, trait_id) != "core::future::future::Future" {
            continue;
        }
        let Some(arg) = args.first().and_then(|arg| arg.node.place()) else {
            continue;
        };
        if referent(cx, body, arg, 0) != Some(place) {
            continue;
        }
        // The poll result and its discriminant must have unique definitions.
        if writes(body, *destination) != 1 {
            continue;
        }
        let next = &body[*target];
        if next
            .statements
            .iter()
            .any(|statement| invalidates(statement, place))
        {
            continue;
        }
        let mir::TerminatorKind::SwitchInt { discr, targets } = &next.terminator().kind else {
            continue;
        };
        let Some(discr) = discr.place() else {
            continue;
        };
        if writes(body, discr) != 1 {
            continue;
        }
        if !next.statements.iter().any(|statement| matches!(
            &statement.kind,
            mir::StatementKind::Assign(assignment)
                if assignment.0 == discr && matches!(assignment.1, mir::Rvalue::Discriminant(p) if p == *destination)
        )) { continue; }
        // Poll::Ready has discriminant zero. Verify the actual defining type.
        let result = destination.ty(&body.local_decls, cx.tcx).ty;
        let ty::Adt(adt, _) = result.kind() else {
            continue;
        };
        if super::canonical_path(cx, adt.did()) != "core::task::poll::Poll" {
            continue;
        }
        if let Some((_, ready)) = targets.iter().find(|(value, _)| *value == 0) {
            ready_edges.push((*target, ready, block));
        }
    }
    if ready_edges.is_empty() {
        return false;
    }
    // None is unreachable. At joins, completion must hold on every incoming
    // execution path. Writes, moves, new mutable aliases and calls invalidate
    // the proof before any subsequent drop can use it.
    let mut entry = vec![None; body.basic_blocks.len()];
    entry[0] = Some(false);
    let mut changed = true;
    while changed {
        changed = false;
        for (block, data) in body.basic_blocks.iter_enumerated() {
            let Some(mut complete) = entry[block.as_usize()] else {
                continue;
            };
            if data
                .statements
                .iter()
                .any(|statement| invalidates(statement, place))
            {
                complete = false;
            }
            match &data.terminator().kind {
                // Arbitrary callees can mutate aliases. Await's direct
                // completed-future drop needs no intervening call.
                mir::TerminatorKind::Call { .. } => complete = false,
                mir::TerminatorKind::Drop { place: dropped, .. } if overlaps(*dropped, place) => {
                    complete = false
                }
                _ => {}
            }
            for successor in data.terminator().successors() {
                let edge = complete
                    || ready_edges
                        .iter()
                        .any(|(source, ready, _)| *source == block && *ready == successor);
                let old = entry[successor.as_usize()];
                let new = Some(old.map_or(edge, |old| old && edge));
                if old != new {
                    entry[successor.as_usize()] = new;
                    changed = true;
                }
            }
        }
    }
    // No statement in this block may replace the value before its drop.
    entry[dropping.as_usize()] == Some(true)
        && !body[dropping]
            .statements
            .iter()
            .any(|statement| invalidates(statement, place))
}

fn invalidates<'tcx>(statement: &mir::Statement<'tcx>, place: mir::Place<'tcx>) -> bool {
    let mir::StatementKind::Assign(assignment) = &statement.kind else {
        return false;
    };
    overlaps(assignment.0, place)
        || assignment
            .0
            .projection
            .iter()
            .any(|part| matches!(part, mir::ProjectionElem::Deref))
        || match &assignment.1 {
            mir::Rvalue::Ref(_, mir::BorrowKind::Mut { .. }, _) => true,
            mir::Rvalue::Use(mir::Operand::Move(moved), _) => overlaps(*moved, place),
            _ => false,
        }
}
fn overlaps<'tcx>(left: mir::Place<'tcx>, right: mir::Place<'tcx>) -> bool {
    left.local == right.local
        && (left.projection.starts_with(right.projection)
            || right.projection.starts_with(left.projection))
}
fn writes<'tcx>(body: &mir::Body<'tcx>, place: mir::Place<'tcx>) -> usize {
    body.basic_blocks
        .iter()
        .map(|block| {
            block
                .statements
                .iter()
                .filter(|statement| {
                    matches!(&statement.kind,
                        mir::StatementKind::Assign(assignment) if overlaps(assignment.0, place)
                    )
                })
                .count()
                + usize::from(matches!(&block.terminator().kind,
                    mir::TerminatorKind::Call { destination, .. } if overlaps(*destination, place)
                ))
        })
        .sum()
}

// Follow only uniquely defined reference/Pin temporaries. Ambiguous aliases
// cannot prove completion. This never follows arbitrary pointer casts.
fn referent<'tcx>(
    cx: &LateContext<'tcx>,
    body: &mir::Body<'tcx>,
    place: mir::Place<'tcx>,
    depth: usize,
) -> Option<mir::Place<'tcx>> {
    if depth >= 16 || writes(body, place) != 1 {
        return None;
    }
    for block in body.basic_blocks.iter() {
        for statement in &block.statements {
            let mir::StatementKind::Assign(assignment) = &statement.kind else {
                continue;
            };
            if assignment.0 != place {
                continue;
            }
            return match &assignment.1 {
                mir::Rvalue::Ref(_, _, borrowed) => {
                    if matches!(
                        borrowed.projection.first(),
                        Some(mir::ProjectionElem::Deref)
                    ) && matches!(body.local_decls[borrowed.local].ty.kind(), ty::Ref(..))
                    {
                        let base = referent(cx, body, mir::Place::from(borrowed.local), depth + 1)?;
                        Some(base.project_deeper(&borrowed.projection[1..], cx.tcx))
                    } else {
                        Some(*borrowed)
                    }
                }
                mir::Rvalue::Use(operand, _) => referent(cx, body, operand.place()?, depth + 1),
                _ => None,
            };
        }
        if let mir::TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } = &block.terminator().kind
        {
            if *destination != place {
                continue;
            }
            let ty::FnDef(id, _) = *func.ty(&body.local_decls, cx.tcx).kind() else {
                return None;
            };
            let path = super::canonical_path(cx, id);
            if !path.starts_with("core::pin::Pin::")
                || !matches!(cx.tcx.item_name(id).as_str(), "new_unchecked" | "new")
            {
                return None;
            }
            return referent(cx, body, args.first()?.node.place()?, depth + 1);
        }
    }
    None
}
