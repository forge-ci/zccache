//! The one-level enumeration exception belongs to a sealed, budgeted cursor.
use rustc_hir::def_id::DefId;
use rustc_lint::LateContext;

pub(super) fn cursor_open(cx: &LateContext<'_>, definition: DefId) -> bool {
    cx.tcx.crate_name(definition.krate).as_str() == "zccache_daemon_core"
        && super::canonical_path(cx, definition)
            .ends_with("::immediate_children::ImmediateChildren::open_for")
        && super::filename(cx, cx.tcx.def_span(definition))
            .ends_with("/persist/staged_store/immediate_children.rs")
}

pub(super) fn approved_caller(cx: &LateContext<'_>, filename: &str) -> bool {
    let Some(owner) = super::enclosing_function(cx) else {
        return false;
    };
    match super::canonical_path(cx, owner).as_str() {
        "zccache_daemon_core::daemon::server::persist::staged_store::maintenance::detach_staged_artifacts_for_clear" =>
            filename.ends_with("/persist/staged_store/maintenance.rs"),
        "zccache_daemon_core::daemon::server::persist::staged_store::clear_detach::detach_clear_artifact_paths_for" =>
            filename.ends_with("/persist/staged_store/clear_detach.rs"),
        _ => false,
    }
}

pub(super) fn raw_read_dir(cx: &LateContext<'_>, value: rustc_middle::ty::Ty<'_>) -> bool {
    let rustc_middle::ty::FnDef(definition, _) = value.kind() else {
        return false;
    };
    cx.tcx.crate_name(definition.krate).as_str() == "std"
        && super::canonical_path(cx, *definition) == "std::fs::read_dir"
}
