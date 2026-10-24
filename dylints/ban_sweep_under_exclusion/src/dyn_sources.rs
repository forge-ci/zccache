//! Trait-object drop effects from the crate's own unsizing sources.
//!
//! Dropping `dyn Trait` runs the destructor of a concrete value that some
//! code unsized into it. Local unsizing coercions and casts name that value
//! exactly. Foreign generic code can also box a local callback internally
//! (tokio runtime hooks, for example), so every local closure or function
//! item passed to a foreign generic function is a source for any trait.
//! Other local types unsized only inside foreign generic code are not
//! tracked; foreign values carry no zccache cleanup effects.
//!
//! A generic source (a helper boxing its `F` parameter) is instantiated
//! through every local use of its function, following generic callers up to
//! a fixed depth. An exported generic function, or one deeper than that
//! bound, keeps the conservative unknown effect. Each trait's result is
//! memoized; a trait reached again while it is being resolved contributes
//! nothing further on that cycle.
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_hir::{intravisit, Expr, ExprKind, HirId};
use rustc_lint::LateContext;
use rustc_middle::ty::{self, TypeVisitableExt};
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Clone, Copy)]
enum Site {
    /// An unsizing adjustment on this expression.
    Adjusted(HirId, usize),
    /// An explicit `as` cast to a trait-object pointer.
    Cast(HirId),
    /// A callback argument of a foreign generic call.
    Callback(HirId),
    /// A use of a local generic function: a path, call or method call.
    Use(HirId),
}

/// Generic callers followed before a source stays unknown.
const INSTANTIATION_DEPTH: usize = 8;

thread_local! {
    // One driver invocation lints one crate. Sites are plain HIR ids; types
    // are recomputed from the cached typeck query on demand.
    static SITES: RefCell<Option<Vec<(LocalDefId, Site)>>> = const { RefCell::new(None) };
    /// `None` while a trait is being resolved, then its effects.
    static RESOLVED: RefCell<HashMap<Option<String>, Option<u8>>> = RefCell::new(HashMap::new());
}

pub(super) fn drop_effects<'tcx>(
    cx: &LateContext<'tcx>,
    object: ty::Ty<'tcx>,
    summaries: &mut Vec<(DefId, ty::GenericArgsRef<'tcx>, Option<u8>)>,
    active: &mut Vec<ty::Ty<'tcx>>,
) -> u8 {
    let principal = principal(cx, object);
    match RESOLVED.with(|resolved| resolved.borrow().get(&principal).copied()) {
        Some(Some(effects)) => return effects,
        Some(None) => return 0,
        None => {}
    }
    RESOLVED.with(|resolved| resolved.borrow_mut().insert(principal.clone(), None));
    let sites = SITES.with(|cell| cell.borrow_mut().get_or_insert_with(|| collect(cx)).clone());
    let mut effects = 0;
    for &(owner, site) in &sites {
        let results = cx.tcx.typeck(owner);
        let source = match site {
            Site::Adjusted(hir_id, index) => {
                let mut before = results.node_type(hir_id);
                let Some(adjustments) = results.adjustments().get(hir_id) else {
                    continue;
                };
                if index > 0 {
                    before = adjustments[index - 1].target;
                }
                unsized_source(cx, before, adjustments[index].target, &principal)
            }
            Site::Cast(hir_id) => {
                let ExprKind::Cast(value, _) = cx.tcx.hir_expect_expr(hir_id).kind else {
                    continue;
                };
                unsized_source(
                    cx,
                    results.expr_ty_adjusted(value),
                    results.node_type(hir_id),
                    &principal,
                )
            }
            Site::Callback(hir_id) => {
                let callback = results.node_type(hir_id);
                callback_implements(cx, callback, object).then_some(callback)
            }
            Site::Use(_) => None,
        };
        let Some(source) = source else {
            continue;
        };
        let mut concrete = Vec::new();
        if instantiate(
            cx,
            &sites,
            owner,
            source,
            INSTANTIATION_DEPTH,
            &mut concrete,
        ) {
            for source in concrete {
                effects |= super::effects::drop_value(cx, source, summaries, active);
            }
        } else {
            effects |= super::effects::UNKNOWN;
        }
    }
    RESOLVED.with(|resolved| resolved.borrow_mut().insert(principal, Some(effects)));
    effects
}

/// Instantiate `value`, written in `owner`'s generics, through local uses of
/// that function. Returns false when some instantiation cannot be named.
fn instantiate<'tcx>(
    cx: &LateContext<'tcx>,
    sites: &[(LocalDefId, Site)],
    owner: LocalDefId,
    value: ty::Ty<'tcx>,
    depth: usize,
    concrete: &mut Vec<ty::Ty<'tcx>>,
) -> bool {
    if !value.has_param() {
        if !concrete.contains(&value) {
            concrete.push(value);
        }
        return true;
    }
    let root = cx.tcx.typeck_root_def_id(owner.to_def_id());
    if depth == 0
        || root
            .as_local()
            .is_none_or(|root| cx.effective_visibilities.is_exported(root))
    {
        super::effects::explain_trace(format_args!(
            "uninstantiated {value} in {root:?} at depth {depth}"
        ));
        return false;
    }
    for &(caller, site) in sites {
        let Site::Use(hir_id) = site else {
            continue;
        };
        let Some((callee, arguments)) = used_function(cx, caller, hir_id) else {
            continue;
        };
        if callee != root {
            continue;
        }
        let value = ty::EarlyBinder::bind(value)
            .instantiate(cx.tcx, arguments)
            .skip_norm_wip();
        if !instantiate(cx, sites, caller, value, depth - 1, concrete) {
            return false;
        }
    }
    true
}

/// The local function and generic arguments a use resolves to, with trait
/// methods resolved to their implementation where the arguments allow it.
fn used_function<'tcx>(
    cx: &LateContext<'tcx>,
    owner: LocalDefId,
    hir_id: HirId,
) -> Option<(DefId, ty::GenericArgsRef<'tcx>)> {
    let results = cx.tcx.typeck(owner);
    let (definition, arguments) = match results.type_dependent_def(hir_id) {
        Some((_, definition)) => (definition, results.node_args(hir_id)),
        None => match results.node_type(hir_id).kind() {
            ty::FnDef(definition, arguments) => (*definition, *arguments),
            _ => return None,
        },
    };
    match ty::Instance::try_resolve(cx.tcx, cx.typing_env(), definition, arguments) {
        Ok(Some(instance)) => Some((instance.def_id(), instance.args)),
        _ => Some((definition, arguments)),
    }
}

/// A trait object's principal trait with its generic arguments, regions
/// erased, so `dyn Fn(&TaskMeta)` and `dyn Fn()` stay distinct.
fn principal<'tcx>(cx: &LateContext<'tcx>, object: ty::Ty<'tcx>) -> Option<String> {
    let ty::Dynamic(predicates, ..) = object.kind() else {
        return None;
    };
    predicates.principal().map(|principal| {
        let principal = cx.tcx.instantiate_bound_regions_with_erased(principal);
        format!("{:?}", cx.tcx.erase_and_anonymize_regions(principal))
    })
}

/// Whether a closure or function item can be unsized into `object`: its
/// principal must be an `Fn`-family trait over the callback's inputs.
/// Generic signatures match conservatively.
fn callback_implements<'tcx>(
    cx: &LateContext<'tcx>,
    callback: ty::Ty<'tcx>,
    object: ty::Ty<'tcx>,
) -> bool {
    let ty::Dynamic(predicates, ..) = object.kind() else {
        return false;
    };
    let Some(principal) = predicates.principal() else {
        return false;
    };
    let principal = cx.tcx.instantiate_bound_regions_with_erased(principal);
    if cx.tcx.fn_trait_kind_from_def_id(principal.def_id).is_none() {
        return false;
    }
    let signature = match callback.kind() {
        ty::Closure(_, arguments) => arguments.as_closure().sig(),
        ty::FnDef(..) => callback.fn_sig(cx.tcx),
        _ => return false,
    };
    let inputs = cx
        .tcx
        .instantiate_bound_regions_with_erased(signature)
        .inputs();
    let inputs = match callback.kind() {
        // A closure signature already packs its inputs into one tuple.
        ty::Closure(..) => inputs.first().copied(),
        _ => Some(ty::Ty::new_tup(cx.tcx, inputs)),
    };
    let expected = principal.args.types().next();
    match (inputs, expected) {
        (Some(inputs), Some(expected)) => {
            inputs.has_param()
                || expected.has_param()
                || cx.tcx.erase_and_anonymize_regions(inputs)
                    == cx.tcx.erase_and_anonymize_regions(expected)
        }
        _ => true,
    }
}

/// The concrete pointee that becomes `dyn principal` in `source -> target`.
fn unsized_source<'tcx>(
    cx: &LateContext<'tcx>,
    source: ty::Ty<'tcx>,
    target: ty::Ty<'tcx>,
    principal: &Option<String>,
) -> Option<ty::Ty<'tcx>> {
    match (source.kind(), target.kind()) {
        (_, ty::Dynamic(..)) => {
            (source != target && self::principal(cx, target) == *principal).then_some(source)
        }
        (ty::Ref(_, source, _), ty::Ref(_, target, _))
        | (ty::RawPtr(source, _), ty::RawPtr(target, _)) => {
            unsized_source(cx, *source, *target, principal)
        }
        (ty::Adt(left, sources), ty::Adt(right, targets)) if left.did() == right.did() => sources
            .types()
            .zip(targets.types())
            .filter(|(source, target)| source != target)
            .find_map(|(source, target)| unsized_source(cx, source, target, principal)),
        _ => None,
    }
}

fn collect(cx: &LateContext<'_>) -> Vec<(LocalDefId, Site)> {
    let mut sites = Vec::new();
    for owner in cx.tcx.hir_body_owners() {
        if !cx.tcx.has_typeck_results(owner) {
            continue;
        }
        let body = cx.tcx.hir_body_owned_by(owner);
        let mut visitor = Collector {
            cx,
            owner,
            results: cx.tcx.typeck(owner),
            sites: &mut sites,
        };
        intravisit::Visitor::visit_body(&mut visitor, body);
    }
    sites
}

struct Collector<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    owner: LocalDefId,
    results: &'tcx ty::TypeckResults<'tcx>,
    sites: &'a mut Vec<(LocalDefId, Site)>,
}

impl<'tcx> intravisit::Visitor<'tcx> for Collector<'_, 'tcx> {
    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        for (index, adjustment) in self.results.expr_adjustments(expr).iter().enumerate() {
            if matches!(
                adjustment.kind,
                ty::adjustment::Adjust::Pointer(ty::adjustment::PointerCoercion::Unsize)
            ) {
                self.sites
                    .push((self.owner, Site::Adjusted(expr.hir_id, index)));
            }
        }
        if let Some((definition, _)) = generic_local_use(self.cx, self.results, expr) {
            if definition.is_local() {
                self.sites.push((self.owner, Site::Use(expr.hir_id)));
            }
        }
        match expr.kind {
            ExprKind::Cast(..) if trait_object_pointer(self.results.expr_ty(expr)) => {
                self.sites.push((self.owner, Site::Cast(expr.hir_id)));
            }
            ExprKind::Call(callee, arguments) => {
                if let ty::FnDef(definition, _) = self.results.expr_ty(callee).kind() {
                    self.callbacks(*definition, arguments);
                }
            }
            ExprKind::MethodCall(_, receiver, arguments, _) => {
                if let Some(definition) = self.results.type_dependent_def_id(expr.hir_id) {
                    self.callbacks(definition, std::slice::from_ref(receiver));
                    self.callbacks(definition, arguments);
                }
            }
            _ => {}
        }
        intravisit::walk_expr(self, expr);
    }
}

impl<'tcx> Collector<'_, 'tcx> {
    fn callbacks(&mut self, callee: DefId, arguments: &'tcx [Expr<'tcx>]) {
        if callee.is_local()
            || !self
                .cx
                .tcx
                .generics_of(callee)
                .requires_monomorphization(self.cx.tcx)
        {
            return;
        }
        for argument in arguments {
            if matches!(
                self.results.expr_ty_adjusted(argument).kind(),
                ty::Closure(..) | ty::FnDef(..)
            ) {
                self.sites
                    .push((self.owner, Site::Callback(argument.hir_id)));
            }
        }
    }
}

fn generic_local_use<'tcx>(
    cx: &LateContext<'tcx>,
    results: &'tcx ty::TypeckResults<'tcx>,
    expr: &Expr<'tcx>,
) -> Option<(DefId, ty::GenericArgsRef<'tcx>)> {
    let (definition, arguments) = match expr.kind {
        ExprKind::MethodCall(..) => (
            results.type_dependent_def_id(expr.hir_id)?,
            results.node_args(expr.hir_id),
        ),
        _ => match results.node_type(expr.hir_id).kind() {
            ty::FnDef(definition, arguments) => (*definition, *arguments),
            _ => return None,
        },
    };
    (!arguments.is_empty() || cx.tcx.generics_of(definition).count() > 0)
        .then_some((definition, arguments))
}

fn trait_object_pointer(value: ty::Ty<'_>) -> bool {
    match value.kind() {
        ty::Dynamic(..) => true,
        ty::Ref(_, value, _) | ty::RawPtr(value, _) => trait_object_pointer(*value),
        ty::Adt(_, arguments) => arguments.types().any(trait_object_pointer),
        _ => false,
    }
}
