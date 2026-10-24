//! Remove execution-impossible drop edges from the lint's private MIR clone.
use rustc_lint::LateContext;
use rustc_middle::mir;
use rustc_mir_dataflow::{
    move_paths::{LookupResult, MoveData},
    Analysis,
};

pub(super) fn prune<'tcx>(
    cx: &LateContext<'tcx>,
    body: &mut mir::Body<'tcx>,
    frame: Option<mir::Local>,
) {
    loop {
        let removed = {
            let data = MoveData::gather_moves(body, cx.tcx, |_| true);
            let mut cursor =
                super::guard_initialization::GuardInitialization::new(cx.tcx, body, &data)
                    .with_frame(frame)
                    .iterate_to_fixpoint(cx.tcx, body, None)
                    .into_results_cursor(body);
            let mut removed = Vec::new();
            for (block, value) in body.basic_blocks.iter_enumerated() {
                let mir::TerminatorKind::Drop { place, target, .. } = value.terminator().kind
                else {
                    continue;
                };
                cursor.seek_before_primary_effect(mir::Location {
                    block,
                    statement_index: value.statements.len(),
                });
                let live = super::drop_paths::live_types(cx, body, place, |place| {
                    match data.rev_lookup.find(place.as_ref()) {
                        LookupResult::Exact(index) | LookupResult::Parent(Some(index)) => {
                            cursor.get().contains(index)
                        }
                        // Untracked storage cannot establish an empty drop.
                        LookupResult::Parent(None) => true,
                    }
                });
                if !live
                    .into_iter()
                    .any(|value| value.needs_drop(cx.tcx, cx.typing_env()))
                {
                    removed.push((block, target));
                }
            }
            removed
        };
        if removed.is_empty() {
            break;
        }
        // Removing an impossible unwind can prove another moved drop empty.
        // Every pass removes at least one Drop; no original MIR is modified.
        for (block, target) in removed {
            body.basic_blocks.as_mut()[block].terminator_mut().kind =
                mir::TerminatorKind::Goto { target };
        }
    }
}
