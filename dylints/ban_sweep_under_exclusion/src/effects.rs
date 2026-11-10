//! Resolved bulk/sweep effects, including local function and closure bodies.
use rustc_hir::def_id::DefId;
use rustc_lint::LateContext;
use rustc_middle::{mir, ty};

pub(super) const BULK: u8 = 1;
pub(super) const SWEEP: u8 = 2;
pub(super) const UNKNOWN: u8 = 4;
pub(super) const ESCAPE: u8 = 8;

pub(super) fn call<'tcx>(
    cx: &LateContext<'tcx>,
    callee: ty::Ty<'tcx>,
    seen: &mut Vec<(DefId, ty::GenericArgsRef<'tcx>, Option<u8>)>,
) -> u8 {
    match callee.kind() {
        ty::FnDef(definition, arguments) => {
            let path = cx.get_def_path(*definition);
            let name = path.last().map(|name| name.as_str());
            if matches!(name, Some("call" | "call_once" | "call_mut"))
                && matches!(cx.tcx.crate_name(definition.krate).as_str(), "core" | "std")
            {
                return match arguments.types().next().map(|value| *value.kind()) {
                    Some(ty::Closure(definition, args)) | Some(ty::FnDef(definition, args)) => {
                        function(cx, definition, args, seen)
                    }
                    _ => UNKNOWN,
                };
            }
            function(cx, *definition, arguments, seen)
        }
        ty::FnPtr(..) => UNKNOWN,
        _ => UNKNOWN,
    }
}

fn function<'tcx>(
    cx: &LateContext<'tcx>,
    definition: DefId,
    arguments: ty::GenericArgsRef<'tcx>,
    seen: &mut Vec<(DefId, ty::GenericArgsRef<'tcx>, Option<u8>)>,
) -> u8 {
    if let Some((_, _, effects)) = seen
        .iter()
        .find(|(id, args, _)| *id == definition && *args == arguments)
    {
        return effects.unwrap_or(0);
    }
    if seen
        .iter()
        .filter(|(_, _, effects)| effects.is_none())
        .count()
        >= 128
    {
        explain_trace(format_args!("call depth limit at {definition:?}"));
        return UNKNOWN;
    }
    let index = seen.len();
    seen.push((definition, arguments, None));
    let result = (|| {
        let path = cx.get_def_path(definition);
        let name = path.last().map(|name| name.as_str()).unwrap_or("");
        let krate = cx.tcx.crate_name(definition.krate);
        if super::guard_escapes::leaks(cx, ty::Ty::new_fn_def(cx.tcx, definition, arguments)) {
            return ESCAPE;
        }
        if matches!(krate.as_str(), "core" | "std")
            && (super::canonical_path(cx, definition) == "core::mem::drop"
                || name == "drop_in_place" && path.iter().any(|part| part.as_str() == "ptr")
                || name == "drop" && path.iter().any(|part| part.as_str() == "ManuallyDrop")
                || name == "assume_init_drop"
                    && path.iter().any(|part| part.as_str() == "MaybeUninit"))
        {
            return arguments
                .types()
                .next()
                .map_or(0, |value| drop_value(cx, value, seen, &mut Vec::new()));
        }
        if matches!(krate.as_str(), "zccache_artifact" | "zccache_daemon_core")
            && (path
                .iter()
                .any(|part| matches!(part.as_str(), "DetachedKvNamespace" | "DetachedStagedTrash"))
                && matches!(name, "sweep_for" | "sweep_until")
                || matches!(
                    name,
                    "sweep_staged_trash_for" | "sweep_retired_namespaces_for"
                ))
        {
            return SWEEP;
        }
        if matches!(krate.as_str(), "std" | "tokio")
            && cx.tcx.def_kind(definition) == rustc_hir::def::DefKind::Fn
            && path.iter().any(|part| part.as_str() == "fs")
            && name == "remove_dir_all"
        {
            return SWEEP;
        }
        if matches!(krate.as_str(), "std" | "tokio")
            && (cx.tcx.def_kind(definition) == rustc_hir::def::DefKind::Fn
                && path.iter().any(|part| part.as_str() == "fs")
                && matches!(name, "read_dir" | "read" | "write" | "copy")
                || matches!(
                    name,
                    "read_to_end" | "read_to_string" | "write_all" | "sync_all" | "sync_data"
                ))
        {
            return BULK;
        }
        let Some(local) = definition.as_local() else {
            // Foreign callback combinators (Iterator::for_each, etc.) do not
            // expose their MIR here. Their concrete local closure bodies do.
            // Conservatively propagate those effects rather than treating the
            // foreign function as a cleanup-free boundary.
            return arguments
                .types()
                .fold(0, |effects, value| match value.kind() {
                    ty::Closure(callback, arguments) => {
                        effects | function(cx, *callback, arguments, seen)
                    }
                    _ => effects,
                });
        };
        if !matches!(
            cx.tcx.def_kind(definition),
            rustc_hir::def::DefKind::Fn
                | rustc_hir::def::DefKind::AssocFn
                | rustc_hir::def::DefKind::Closure
        ) {
            return 0;
        }
        // Required trait methods have no MIR body. Their dynamic implementation
        // cannot be assumed free of cleanup, and querying MIR would ICE rustc.
        if cx.tcx.hir_maybe_body_owned_by(local).is_none() {
            explain_trace(format_args!("bodiless callee {definition:?}"));
            return UNKNOWN;
        }
        // Release the query borrow before following callees: a query for a callee
        // may legitimately advance/consume MIR for an enclosing function.
        let calls = {
            let (stored, _) = cx.tcx.mir_promoted(local);
            if stored.is_stolen() {
                targets(cx, cx.tcx.optimized_mir(local).clone(), false)
            } else {
                {
                    let body = (*stored.borrow()).clone();
                    targets(cx, body, true)
                }
            }
        };
        calls.into_iter().fold(0, |effects, target| {
            if let Target::Erase(source, destination) = target {
                let source = ty::EarlyBinder::bind(source).instantiate(cx.tcx, arguments).skip_norm_wip();
                let destination = ty::EarlyBinder::bind(destination).instantiate(cx.tcx, arguments).skip_norm_wip();
                let owns = |value| super::guard_types::contains(cx, value, super::guard_types::Scope::Ownership, &mut Vec::new());
                return effects | if owns(source) && !owns(destination) { ESCAPE } else { 0 };
            }
            // Only this sealed cursor's direct one-level directory open is
            // metadata work. Every other effect in its body/helper graph
            // remains subject to the normal exclusion policy.
            if super::detach_policy::cursor_open(cx, definition)
                && matches!(target, Target::Call(value) if super::detach_policy::raw_read_dir(cx, value))
            {
                return effects;
            }
            let (value, dropping) = match target {
                Target::Call(value) => (value, false),
                Target::Drop(value) => (value, true),
                Target::Erase(..) => unreachable!(),
            };
            let value = ty::EarlyBinder::bind(value)
                .instantiate(cx.tcx, arguments)
                .skip_norm_wip();
            effects | if dropping {
                drop_value(cx, value, seen, &mut Vec::new())
            } else { call(cx, value, seen) }
        })
    })();
    seen[index].2 = Some(result);
    result
}

enum Target<'tcx> {
    Call(ty::Ty<'tcx>),
    Drop(ty::Ty<'tcx>),
    Erase(ty::Ty<'tcx>, ty::Ty<'tcx>),
}
fn targets<'tcx>(
    cx: &LateContext<'tcx>,
    mut owned: mir::Body<'tcx>,
    precise: bool,
) -> Vec<Target<'tcx>> {
    use rustc_mir_dataflow::{
        move_paths::{LookupResult, MoveData},
        Analysis,
    };
    if !precise {
        // Optimized MIR already has real drop elaboration. Do not feed its
        // Deinit/SetDiscriminant statements into pre-borrowck MoveData.
        let mut targets: Vec<_> = owned
            .basic_blocks
            .iter()
            .filter_map(|block| match &block.terminator().kind {
                mir::TerminatorKind::Call { func, .. } => {
                    Some(Target::Call(func.ty(&owned.local_decls, cx.tcx)))
                }
                mir::TerminatorKind::Drop { place, .. } => {
                    Some(Target::Drop(place.ty(&owned.local_decls, cx.tcx).ty))
                }
                _ => None,
            })
            .collect();
        targets.extend(casts(cx, &owned));
        return targets;
    }
    for block in owned.basic_blocks.as_mut() {
        if let Some(terminator) = &mut block.terminator {
            if let mir::TerminatorKind::FalseEdge { real_target, .. }
            | mir::TerminatorKind::FalseUnwind { real_target, .. } = terminator.kind
            {
                terminator.kind = mir::TerminatorKind::Goto {
                    target: real_target,
                };
            }
        }
    }
    let body = &owned;
    let data = MoveData::gather_moves(body, cx.tcx, |_| true);
    let mut cursor = super::guard_initialization::GuardInitialization::new(cx.tcx, body, &data)
        .iterate_to_fixpoint(cx.tcx, body, None)
        .into_results_cursor(body);
    let mut result = Vec::new();
    for (block, value) in body.basic_blocks.iter_enumerated() {
        match &value.terminator().kind {
            mir::TerminatorKind::Call { func, .. } => {
                result.push(Target::Call(func.ty(&body.local_decls, cx.tcx)))
            }
            mir::TerminatorKind::Drop { place, .. } => {
                cursor.seek_before_primary_effect(mir::Location {
                    block,
                    statement_index: value.statements.len(),
                });
                let live = super::drop_paths::live_types(cx, body, *place, |place| {
                    match data.rev_lookup.find(place.as_ref()) {
                        LookupResult::Exact(index) | LookupResult::Parent(Some(index)) => {
                            cursor.get().contains(index)
                        }
                        LookupResult::Parent(None) => true,
                    }
                });
                result.extend(live.into_iter().map(Target::Drop));
            }
            _ => {}
        }
    }
    result.extend(casts(cx, body));
    result
}

fn casts<'tcx>(cx: &LateContext<'tcx>, body: &mir::Body<'tcx>) -> Vec<Target<'tcx>> {
    body.basic_blocks
        .iter()
        .flat_map(|block| &block.statements)
        .filter_map(|statement| {
            let mir::StatementKind::Assign(assignment) = &statement.kind else {
                return None;
            };
            let mir::Rvalue::Cast(_, operand, destination) = &assignment.1 else {
                return None;
            };
            Some(Target::Erase(
                operand.ty(&body.local_decls, cx.tcx),
                *destination,
            ))
        })
        .collect()
}

pub(super) fn drop_value<'tcx>(
    cx: &LateContext<'tcx>,
    value: ty::Ty<'tcx>,
    summaries: &mut Vec<(DefId, ty::GenericArgsRef<'tcx>, Option<u8>)>,
    active: &mut Vec<ty::Ty<'tcx>>,
) -> u8 {
    if matches!(value.kind(), ty::Alias(..)) {
        if let Ok(normalized) = cx
            .tcx
            .try_normalize_erasing_regions(cx.typing_env(), ty::Unnormalized::new_wip(value))
        {
            if normalized != value {
                return drop_value(cx, normalized, summaries, active);
            }
        }
    }
    if active.contains(&value) {
        return 0;
    }
    if active.len() >= 128 {
        explain_trace(format_args!("drop depth limit at {value}"));
        return UNKNOWN;
    }
    active.push(value);
    let effects = match value.kind() {
        ty::Adt(definition, arguments) => {
            let name = cx.tcx.item_name(definition.did());
            let standard = matches!(
                cx.tcx.crate_name(definition.did().krate).as_str(),
                "core" | "std" | "alloc"
            );
            if super::guard_types::direct(cx, definition.did()).is_some()
                || standard && name.as_str() == "ManuallyDrop"
            {
                0
            } else if standard && name.as_str() == "Weak" {
                // Weak drops its allocator, never the strong owner's value.
                arguments.types().skip(1).fold(0, |effects, field| {
                    effects | drop_value(cx, field, summaries, active)
                })
            } else {
                let own = definition
                    .destructor(cx.tcx)
                    .filter(|drop| drop.did.is_local())
                    .map_or(0, |drop| function(cx, drop.did, arguments, summaries));
                if own & (SWEEP | UNKNOWN) != 0 {
                    explain_trace(format_args!("destructor {value} effects {own}"));
                }
                let fields =
                    if definition.did().is_local() || definition.destructor(cx.tcx).is_none() {
                        definition.all_fields().fold(0, |effects, field| {
                            effects
                                | drop_value(
                                    cx,
                                    field.ty(cx.tcx, arguments).skip_norm_wip(),
                                    summaries,
                                    active,
                                )
                        })
                    } else {
                        arguments.types().fold(0, |effects, field| {
                            effects | drop_value(cx, field, summaries, active)
                        })
                    };
                own | fields
            }
        }
        ty::Tuple(fields) => fields.iter().fold(0, |effects, field| {
            effects | drop_value(cx, field, summaries, active)
        }),
        ty::Closure(_, arguments) => arguments
            .as_closure()
            .upvar_tys()
            .iter()
            .fold(0, |effects, field| {
                effects | drop_value(cx, field, summaries, active)
            }),
        // An unpolled future already owns its captures. Cancelling it drops
        // those values even though none of its body has executed.
        ty::Coroutine(definition, arguments) => {
            let captures = arguments
                .as_coroutine()
                .upvar_tys()
                .iter()
                .fold(0, |effects, field| {
                    effects | drop_value(cx, field, summaries, active)
                });
            // Cancellation after suspension also drops initialized saved
            // locals. Include every possible state: the caller generally
            // cannot prove which poll state an owned future has reached.
            if definition.is_local() || cx.tcx.is_mir_available(*definition) {
                let body = cx.tcx.optimized_mir(*definition);
                let saved = body
                    .coroutine
                    .as_ref()
                    .and_then(|value| value.coroutine_layout.as_ref())
                    .map_or(0, |layout| {
                        layout.field_tys.iter().fold(0, |effects, field| {
                            let value = ty::EarlyBinder::bind(field.ty)
                                .instantiate(cx.tcx, arguments)
                                .skip_norm_wip();
                            effects | drop_value(cx, value, summaries, active)
                        })
                    });
                captures | saved
            } else {
                captures | UNKNOWN
            }
        }
        ty::Array(field, _) | ty::Slice(field) => drop_value(cx, *field, summaries, active),
        ty::Alias(alias) if matches!(alias.kind, ty::Opaque { .. }) => {
            let hidden = cx
                .tcx
                .type_of(alias.kind.def_id())
                .instantiate(cx.tcx, alias.args)
                .skip_norm_wip();
            if hidden == value {
                UNKNOWN
            } else {
                drop_value(cx, hidden, summaries, active)
            }
        }
        ty::Dynamic(..) => super::dyn_sources::drop_effects(cx, value, summaries, active),
        ty::Param(..) | ty::Alias(..) => {
            explain_trace(format_args!(
                "opaque drop {value} via {}",
                active
                    .iter()
                    .map(|outer| outer.to_string())
                    .collect::<Vec<_>>()
                    .join(" > ")
            ));
            UNKNOWN
        }
        _ => 0,
    };
    active.pop();
    effects
}

/// `ZCCACHE_DYLINT_EXPLAIN_TRACE=<file>` appends which destructors carry
/// sweep or unknown effects; cargo does not forward raw driver stderr.
pub(super) fn explain_trace(line: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let Some(path) = std::env::var_os("ZCCACHE_DYLINT_EXPLAIN_TRACE") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}
