//! Ownership-sensitive guard roles. Non-owning type parameters are not locks.
use rustc_lint::LateContext;
use rustc_middle::ty;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    Critical,
    Any,
    /// Includes data-free capabilities that must not be leaked, but do not
    /// exclude ordinary filesystem work or suspension.
    Ownership,
}

pub(super) fn direct(cx: &LateContext<'_>, definition: rustc_hir::def_id::DefId) -> Option<Scope> {
    let name = cx.tcx.item_name(definition);
    let krate = cx.tcx.crate_name(definition.krate);
    if super::canonical_path(cx, definition) == "zccache_daemon_core::daemon::server::persist::staged_store::read_guard::StagedMaterializationGuard" {
        return Some(Scope::Any);
    }
    if krate.as_str() == "zccache_core"
        && name.as_str() == "CacheRootWriterLease"
        && super::canonical_path(cx, definition)
            == "zccache_core::cache_root_lock::CacheRootWriterLease"
    {
        return Some(Scope::Ownership);
    }
    match krate.as_str() {
        "zccache_daemon_core"
            if name.as_str() == "MaterializationPermit"
                && cx.tcx.opt_parent(definition).is_some_and(|parent| {
                    cx.tcx.item_name(parent).as_str() == "spawn_exclusion"
                }) =>
        {
            Some(Scope::Any)
        }
        "zccache_core" => match name.as_str() {
            "PublicationCommitPermit" | "MutexGuard" | "RwLockReadGuard" | "RwLockWriteGuard" => {
                Some(Scope::Critical)
            }
            "PublicationReadPermit" | "PublicationDrainPermit" => Some(Scope::Any),
            _ => None,
        },
        "zccache_artifact"
            if cx
                .tcx
                .opt_parent(definition)
                .is_some_and(|parent| cx.tcx.item_name(parent).as_str() == "staged_lock") =>
        {
            match name.as_str() {
                "StagedCommitGuard" => Some(Scope::Critical),
                "StagedReadGuard" => Some(Scope::Any),
                _ => None,
            }
        }
        "std" | "kernal_api" | "tokio" | "lock_api" => match name.as_str() {
            "MutexGuard" | "MappedMutexGuard" | "RwLockReadGuard" | "RwLockWriteGuard" => {
                Some(Scope::Critical)
            }
            "FileLock" | "OwnedFileLock" => Some(Scope::Any),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn contains<'tcx>(
    cx: &LateContext<'tcx>,
    value: ty::Ty<'tcx>,
    scope: Scope,
    seen: &mut Vec<ty::Ty<'tcx>>,
) -> bool {
    if seen.contains(&value) {
        return false;
    }
    seen.push(value);
    match value.kind() {
        ty::Ref(_, value, _) => contains(cx, *value, scope, seen),
        ty::Tuple(values) => values.iter().any(|value| contains(cx, value, scope, seen)),
        ty::Array(value, _) | ty::Slice(value) => contains(cx, *value, scope, seen),
        ty::Closure(_, arguments) => arguments
            .as_closure()
            .upvar_tys()
            .iter()
            .any(|value| contains(cx, value, scope, seen)),
        ty::Coroutine(_, arguments) => arguments
            .as_coroutine()
            .upvar_tys()
            .iter()
            .any(|value| contains(cx, value, scope, seen)),
        ty::Adt(definition, arguments) => {
            if let Some(role) = direct(cx, definition.did()) {
                return match role {
                    Scope::Critical => true,
                    Scope::Any => scope != Scope::Critical,
                    Scope::Ownership => scope == Scope::Ownership,
                };
            }
            let name = cx.tcx.item_name(definition.did());
            let krate = cx.tcx.crate_name(definition.did().krate);
            if matches!(krate.as_str(), "core" | "std" | "alloc")
                && matches!(name.as_str(), "Weak" | "PhantomData" | "NonNull")
            {
                return false;
            }
            if scope != Scope::Critical
                && krate.as_str() == "zccache_daemon_core"
                && name.as_str() == "StagingRoot"
                && cx
                    .tcx
                    .opt_parent(definition.did())
                    .is_some_and(|parent| cx.tcx.item_name(parent).as_str() == "staging_root")
            {
                return false;
            }
            if definition.did().is_local() {
                definition.all_fields().any(|field| {
                    contains(cx, field.ty(cx.tcx, arguments).skip_norm_wip(), scope, seen)
                })
            } else {
                arguments
                    .types()
                    .any(|value| contains(cx, value, scope, seen))
            }
        }
        // Raw pointers and function signatures do not retain their pointees/inputs.
        _ => false,
    }
}

/// A borrowed exclusion remains held by its caller even after its reference
/// wrapper is dropped. Sealed guard internals are owned releases, not borrows.
pub(super) fn borrowed<'tcx>(
    cx: &LateContext<'tcx>,
    value: ty::Ty<'tcx>,
    scope: Scope,
    seen: &mut Vec<ty::Ty<'tcx>>,
) -> bool {
    if seen.contains(&value) {
        return false;
    }
    seen.push(value);
    match value.kind() {
        ty::Ref(_, value, _) => contains(cx, *value, scope, &mut Vec::new()),
        ty::Tuple(values) => values.iter().any(|value| borrowed(cx, value, scope, seen)),
        ty::Array(value, _) | ty::Slice(value) => borrowed(cx, *value, scope, seen),
        ty::Closure(_, args) => args
            .as_closure()
            .upvar_tys()
            .iter()
            .any(|value| borrowed(cx, value, scope, seen)),
        ty::Coroutine(_, args) => args
            .as_coroutine()
            .upvar_tys()
            .iter()
            .any(|value| borrowed(cx, value, scope, seen)),
        ty::Adt(definition, args) => {
            if direct(cx, definition.did()).is_some() {
                return false;
            }
            let name = cx.tcx.item_name(definition.did());
            if matches!(
                cx.tcx.crate_name(definition.did().krate).as_str(),
                "core" | "std" | "alloc"
            ) && matches!(name.as_str(), "Weak" | "PhantomData" | "NonNull")
            {
                return false;
            }
            if definition.did().is_local() {
                definition
                    .all_fields()
                    .any(|field| borrowed(cx, field.ty(cx.tcx, args).skip_norm_wip(), scope, seen))
            } else {
                args.types().any(|value| borrowed(cx, value, scope, seen))
            }
        }
        _ => false,
    }
}
