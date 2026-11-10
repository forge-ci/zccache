//! Compiler move/drop effects with enum-edge refinement enabled for this lint.
//! rustc gates its own enum refinement on a code-generation experiment. The
//! lint must preserve Result/Option branch knowledge without changing that flag.
use rustc_abi::VariantIdx;
use rustc_middle::{mir, ty};
use rustc_mir_dataflow::{
    impls::MaybeInitializedPlaces, move_paths::MoveData, Analysis, GenKill, SwitchTargetIndex,
};

pub(super) struct GuardInitialization<'a, 'tcx> {
    inner: MaybeInitializedPlaces<'a, 'tcx>,
    body: &'a mir::Body<'tcx>,
    data: &'a MoveData<'tcx>,
    tcx: ty::TyCtxt<'tcx>,
    frame: Option<mir::Local>,
}
impl<'a, 'tcx> GuardInitialization<'a, 'tcx> {
    pub(super) fn new(
        tcx: ty::TyCtxt<'tcx>,
        body: &'a mir::Body<'tcx>,
        data: &'a MoveData<'tcx>,
    ) -> Self {
        Self {
            inner: MaybeInitializedPlaces::new(tcx, body, data),
            body,
            data,
            tcx,
            frame: None,
        }
    }
    pub(super) fn with_frame(mut self, frame: Option<mir::Local>) -> Self {
        self.frame = frame;
        self
    }
}
impl<'a, 'tcx> Analysis<'tcx> for GuardInitialization<'a, 'tcx> {
    type Domain = <MaybeInitializedPlaces<'a, 'tcx> as Analysis<'tcx>>::Domain;
    type SwitchIntData = (mir::Place<'tcx>, Vec<VariantIdx>);
    const NAME: &'static str = "initialized_exclusion_guards";
    fn bottom_value(&self, body: &mir::Body<'tcx>) -> Self::Domain {
        self.inner.bottom_value(body)
    }
    fn initialize_start_block(&self, body: &mir::Body<'tcx>, state: &mut Self::Domain) {
        self.inner.initialize_start_block(body, state);
        if let Some(frame) = self.frame {
            for (index, path) in self.data.move_paths.iter_enumerated() {
                if path.place.local == frame {
                    state.gen_(index);
                }
            }
        }
    }
    fn apply_primary_statement_effect(
        &self,
        state: &mut Self::Domain,
        statement: &mir::Statement<'tcx>,
        location: mir::Location,
    ) {
        self.inner
            .apply_primary_statement_effect(state, statement, location);
    }
    fn apply_primary_terminator_effect<'mir>(
        &self,
        state: &mut Self::Domain,
        terminator: &'mir mir::Terminator<'tcx>,
        location: mir::Location,
    ) -> mir::TerminatorEdges<'mir, 'tcx> {
        self.inner
            .apply_primary_terminator_effect(state, terminator, location)
    }
    fn apply_call_return_effect(
        &self,
        state: &mut Self::Domain,
        block: mir::BasicBlock,
        places: mir::CallReturnPlaces<'_, 'tcx>,
    ) {
        self.inner.apply_call_return_effect(state, block, places);
    }
    fn get_switch_int_data(
        &self,
        block: mir::BasicBlock,
        targets: &mir::SwitchTargets,
        discr: &mir::Operand<'tcx>,
    ) -> Option<Self::SwitchIntData> {
        let discr = discr.place()?;
        for statement in self.body[block].statements.iter().rev() {
            let mir::StatementKind::Assign(assignment) = &statement.kind else {
                continue;
            };
            let (target, value) = &**assignment;
            if *target != discr {
                continue;
            }
            let mir::Rvalue::Discriminant(place) = value else {
                return None;
            };
            let value = place.ty(&self.body.local_decls, self.tcx).ty;
            let ty::Adt(definition, _) = value.kind() else {
                return None;
            };
            if !definition.is_enum() {
                return None;
            }
            let mut variants = Vec::new();
            for (value, _) in targets.iter() {
                variants.push(definition.discriminants(self.tcx).find_map(
                    |(index, discriminant)| (discriminant.val == value).then_some(index),
                )?);
            }
            return Some((*place, variants));
        }
        None
    }
    fn apply_switch_int_edge_effect(
        &self,
        state: &mut Self::Domain,
        data: &mut Self::SwitchIntData,
        target: SwitchTargetIndex,
    ) {
        let (place, variants) = data;
        for (index, path) in self.data.move_paths.iter_enumerated() {
            if path.place.local != place.local
                || !path.place.projection.starts_with(place.projection)
            {
                continue;
            }
            let Some(mir::ProjectionElem::Downcast(_, variant)) =
                path.place.projection.get(place.projection.len())
            else {
                continue;
            };
            let inactive = match target {
                SwitchTargetIndex::Normal(target) => variants[target] != *variant,
                SwitchTargetIndex::Otherwise => variants.contains(variant),
            };
            if inactive {
                state.kill(index);
            }
        }
    }
}

impl<'tcx> rustc_mir_dataflow::move_paths::HasMoveData<'tcx> for GuardInitialization<'_, 'tcx> {
    fn move_data(&self) -> &MoveData<'tcx> {
        self.data
    }
}
