//! Analysis-only representation of runtime coroutine frame ownership.
//! No rewritten body is returned to rustc or used for code generation.
use rustc_lint::LateContext;
use rustc_middle::{
    mir::{self, visit::MutVisitor},
    ty,
};

pub(super) struct Prepared<'tcx> {
    pub body: mir::Body<'tcx>,
    pub frame: Option<mir::Local>,
    pub aliases: Vec<mir::Local>,
    pub suspensions: Vec<mir::BasicBlock>,
}

pub(super) fn prepare<'tcx>(
    cx: &LateContext<'tcx>,
    mut body: mir::Body<'tcx>,
) -> Result<Prepared<'tcx>, &'static str> {
    for declaration in &mut body.local_decls {
        if matches!(declaration.local_info, mir::ClearCrossCrate::Clear) {
            declaration.local_info = mir::ClearCrossCrate::Set(Box::new(mir::LocalInfo::Boring));
        }
    }
    let mut aliases = Vec::new();
    let mut frame = None;
    let mut suspensions = Vec::new();
    if body.coroutine.is_some() {
        let self_local = mir::Local::from_usize(1);
        let self_ty = body.local_decls[self_local].ty;
        let frame_ty =
            frame_pointee(cx, self_ty).ok_or("cannot identify coroutine frame ownership")?;
        aliases.push(self_local);
        // Follow only copies of the actual self pointer, never other references
        // that happen to have the same coroutine type.
        loop {
            let previous = aliases.len();
            for block in body.basic_blocks.iter() {
                for statement in &block.statements {
                    let mir::StatementKind::Assign(assignment) = &statement.kind else {
                        continue;
                    };
                    let (destination, value) = &**assignment;
                    let Some(destination) = destination.as_local() else {
                        continue;
                    };
                    if frame_pointee(cx, body.local_decls[destination].ty) != Some(frame_ty) {
                        continue;
                    }
                    let source = match value {
                        mir::Rvalue::Use(operand, _) => operand.place(),
                        mir::Rvalue::CopyForDeref(place) => Some(*place),
                        _ => None,
                    };
                    if source.is_some_and(|place| {
                        aliases.contains(&place.local)
                            && !place
                                .projection
                                .iter()
                                .any(|part| matches!(part, mir::ProjectionElem::Deref))
                    }) && !aliases.contains(&destination)
                    {
                        aliases.push(destination);
                    }
                }
            }
            if previous == aliases.len() {
                break;
            }
        }
        // A local that ever points elsewhere is not a proven frame alias.
        // Validate all definitions, including writes to Pin's pointer field.
        for block in body.basic_blocks.iter() {
            for statement in &block.statements {
                let mir::StatementKind::Assign(assignment) = &statement.kind else {
                    continue;
                };
                let (destination, value) = &**assignment;
                if aliases.contains(&destination.local)
                    && !destination
                        .projection
                        .iter()
                        .any(|part| matches!(part, mir::ProjectionElem::Deref))
                {
                    let source = match value {
                        mir::Rvalue::Use(operand, _) => operand.place(),
                        mir::Rvalue::CopyForDeref(place) => Some(*place),
                        _ => None,
                    };
                    if !source.is_some_and(|place| {
                        aliases.contains(&place.local)
                            && !place
                                .projection
                                .iter()
                                .any(|part| matches!(part, mir::ProjectionElem::Deref))
                    }) {
                        return Err("coroutine frame alias has an unproven reassignment");
                    }
                }
                if let mir::Rvalue::Ref(_, _, place) | mir::Rvalue::RawPtr(_, place) = value {
                    if aliases.contains(&place.local)
                        && !place
                            .projection
                            .iter()
                            .any(|part| matches!(part, mir::ProjectionElem::Deref))
                    {
                        return Err("coroutine frame pointer storage escapes by reference");
                    }
                }
            }
            if let mir::TerminatorKind::Call { destination, .. } = &block.terminator().kind {
                if aliases.contains(&destination.local)
                    && !destination
                        .projection
                        .iter()
                        .any(|part| matches!(part, mir::ProjectionElem::Deref))
                {
                    return Err("coroutine frame alias has an unproven call result");
                }
            }
        }
        let owned = body
            .local_decls
            .push(mir::LocalDecl::new(frame_ty, body.span));
        let layout = body
            .coroutine
            .as_ref()
            .unwrap()
            .coroutine_layout
            .as_ref()
            .ok_or("cannot identify coroutine saved-local layout")?
            .clone();
        let storage: rustc_index::IndexVec<_, mir::Local> = layout
            .field_tys
            .iter()
            .map(|field| {
                body.local_decls
                    .push(mir::LocalDecl::new(field.ty, body.span))
            })
            .collect();
        let saved_fields = layout
            .variant_fields
            .iter()
            .map(|fields| fields.iter().map(|saved| storage[*saved]).collect())
            .collect();
        let mut rewrite = FrameRewrite {
            cx,
            declarations: body.local_decls.clone(),
            frame_ty,
            frame: owned,
            aliases: &aliases,
            saved_fields,
            unresolved: false,
        };
        rewrite.visit_body(&mut body);
        if rewrite.unresolved {
            return Err("cannot resolve coroutine frame storage alias");
        }
        link_resumes(&mut body, owned, &mut suspensions)?;
        frame = Some(owned);
    }
    // Borrowck's move builder requires these hints, which runtime MIR erases.
    // Saved locals now have stable storage identities across all states.
    // Discriminants select control flow; they do not initialize/drop values.
    for block in body.basic_blocks.as_mut() {
        for statement in &mut block.statements {
            if matches!(statement.kind, mir::StatementKind::SetDiscriminant { .. }) {
                statement.kind = mir::StatementKind::Nop;
            } else if let mir::StatementKind::Assign(assignment) = &mut statement.kind {
                if let mir::Rvalue::CopyForDeref(place) = assignment.1 {
                    assignment.1 = mir::Rvalue::Use(mir::Operand::Copy(place), mir::WithRetag::No);
                }
            }
        }
    }
    Ok(Prepared {
        body,
        frame,
        aliases,
        suspensions,
    })
}

fn frame_pointee<'tcx>(cx: &LateContext<'tcx>, value: ty::Ty<'tcx>) -> Option<ty::Ty<'tcx>> {
    match value.kind() {
        ty::Ref(_, pointee, _) | ty::RawPtr(pointee, _)
            if matches!(pointee.kind(), ty::Coroutine(..)) =>
        {
            Some(*pointee)
        }
        ty::Adt(definition, args)
            if cx.tcx.crate_name(definition.did().krate).as_str() == "core"
                && cx.tcx.item_name(definition.did()).as_str() == "Pin" =>
        {
            args.types()
                .next()
                .and_then(|value| frame_pointee(cx, value))
        }
        _ => None,
    }
}

struct FrameRewrite<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    declarations: rustc_index::IndexVec<mir::Local, mir::LocalDecl<'tcx>>,
    frame_ty: ty::Ty<'tcx>,
    frame: mir::Local,
    aliases: &'a [mir::Local],
    saved_fields: rustc_index::IndexVec<
        rustc_abi::VariantIdx,
        rustc_index::IndexVec<rustc_abi::FieldIdx, mir::Local>,
    >,
    unresolved: bool,
}
impl<'tcx> MutVisitor<'tcx> for FrameRewrite<'_, 'tcx> {
    fn tcx(&self) -> ty::TyCtxt<'tcx> {
        self.cx.tcx
    }
    fn visit_place(
        &mut self,
        place: &mut mir::Place<'tcx>,
        context: mir::visit::PlaceContext,
        location: mir::Location,
    ) {
        for (index, part) in place.projection.iter().enumerate() {
            if !matches!(part, mir::ProjectionElem::Deref) {
                continue;
            }
            let prefix = mir::Place::from(place.local)
                .project_deeper(&place.projection[..index], self.cx.tcx);
            if frame_pointee(self.cx, prefix.ty(&self.declarations, self.cx.tcx).ty)
                != Some(self.frame_ty)
            {
                continue;
            }
            if self.aliases.contains(&place.local) {
                let tail = &place.projection[index + 1..];
                if let [mir::ProjectionElem::Downcast(_, variant), mir::ProjectionElem::Field(field, _), remaining @ ..] =
                    tail
                {
                    if let Some(local) = self
                        .saved_fields
                        .get(*variant)
                        .and_then(|fields| fields.get(*field))
                    {
                        *place = mir::Place::from(*local).project_deeper(remaining, self.cx.tcx);
                    } else {
                        self.unresolved = true;
                    }
                } else if matches!(tail.first(), Some(mir::ProjectionElem::Downcast(..))) {
                    self.unresolved = true;
                } else {
                    *place = mir::Place::from(self.frame).project_deeper(tail, self.cx.tcx);
                }
            } else {
                self.unresolved = true;
            }
            break;
        }
        self.super_place(place, context, location);
    }
}

/// Analyze the complete lifetime, starting unresumed. Each suspension resumes
/// only at its matching continuation. This avoids conflating unrelated states.
fn link_resumes(
    body: &mut mir::Body<'_>,
    frame: mir::Local,
    suspensions: &mut Vec<mir::BasicBlock>,
) -> Result<(), &'static str> {
    let mut dispatcher = None;
    for (block, data) in body.basic_blocks.iter_enumerated() {
        let mir::TerminatorKind::SwitchInt { discr, targets } = &data.terminator().kind else {
            continue;
        };
        let Some(discr) = discr.place() else {
            continue;
        };
        if data.statements.iter().any(|statement| matches!(&statement.kind,
            mir::StatementKind::Assign(assignment) if assignment.0 == discr
                && matches!(&assignment.1, mir::Rvalue::Discriminant(place) if *place == mir::Place::from(frame)))) {
            if dispatcher.is_some() { return Err("coroutine has ambiguous resume dispatch"); }
            dispatcher = Some((block, targets.iter().collect::<Vec<_>>()));
        }
    }
    let (dispatcher, targets) = dispatcher.ok_or("cannot identify coroutine resume dispatch")?;
    let initial = targets
        .iter()
        .find_map(|(value, block)| (*value == 0).then_some(*block))
        .ok_or("coroutine has no unresumed entry")?;
    body.basic_blocks.as_mut()[dispatcher].terminator_mut().kind =
        mir::TerminatorKind::Goto { target: initial };
    for (block, data) in body.basic_blocks.as_mut().iter_enumerated_mut() {
        if !matches!(data.terminator().kind, mir::TerminatorKind::Return) {
            continue;
        }
        let state = data
            .statements
            .iter()
            .find_map(|statement| match &statement.kind {
                mir::StatementKind::SetDiscriminant {
                    place,
                    variant_index,
                } if **place == mir::Place::from(frame)
                    && *variant_index >= rustc_abi::VariantIdx::from_usize(3) =>
                {
                    Some(*variant_index)
                }
                _ => None,
            });
        if let Some(state) = state {
            let target = targets
                .iter()
                .find_map(|(value, block)| (*value == u128::from(state.as_u32())).then_some(*block))
                .ok_or("coroutine suspension has no matching continuation")?;
            suspensions.push(block);
            data.terminator_mut().kind = mir::TerminatorKind::Goto { target };
        }
    }
    Ok(())
}
