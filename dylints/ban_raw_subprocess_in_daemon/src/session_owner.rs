//! Resolve process capabilities by their defining crate and type.

use rustc_hir::def::DefKind;
use rustc_hir::def_id::DefId;
use rustc_lint::LateContext;
use rustc_middle::ty;
use rustc_span::Span;

pub(super) fn check(cx: &LateContext<'_>, span: Span, def_id: DefId) -> bool {
    let path = cx.get_def_path(def_id);
    let parts: Vec<_> = path.iter().map(|part| part.as_str().to_string()).collect();
    let Some(name) = parts.last().map(String::as_str) else {
        return false;
    };
    let crate_name = cx.tcx.crate_name(def_id.krate);
    let defining_crate = crate_name.as_str();
    let semantic = defining_crate == "kernal_api"
        && (parts.iter().any(|part| part == "SpawnSpec")
            && matches!(name, "spawn" | "spawn_session")
            || parts.iter().any(|part| {
                matches!(
                    part.as_str(),
                    "ProcessSession"
                        | "PlatformChild"
                        | "SpawnedChild"
                        | "DaemonChild"
                        | "SpawnHandle"
                )
            }) && matches!(
                name,
                "wait"
                    | "try_wait"
                    | "next_output"
                    | "wait_with_output_bounded"
                    | "kill"
                    | "poll"
                    | "write_stdin"
                    | "close_stdin"
                    | "shutdown_output"
                    | "cpu_time"
                    | "terminate_group_soft"
            )
            || matches!(
                name,
                "run_bounded_command"
                    | "run_bounded_command_async"
                    | "foreground_output"
                    | "foreground_status"
                    | "spawn_sync"
                    | "spawn_sync_daemon"
                    | "spawn_with_options"
            ));
    let native_wait = matches!(defining_crate, "std" | "tokio")
        && parts.iter().any(|part| part == "process")
        && parts.iter().any(|part| part == "Child")
        && matches!(
            name,
            "wait" | "wait_with_output" | "kill" | "start_kill" | "try_wait"
        );
    if !semantic && !native_wait {
        return false;
    }
    let filename = super::normalize_slashes(&super::source_filename(cx, span));
    let owner = filename.ends_with("crates/zccache-daemon-core/src/daemon/process_session.rs");
    if semantic && owner && enclosing_function_has_budget(cx) {
        return true;
    }
    // Native implementations have platform-specific DefPaths; diagnostics
    // name the stable public facade so UI expectations apply on every host.
    let display_path = if matches!(name, "spawn_sync" | "spawn_sync_daemon") {
        format!("kernal_api::platform::process::{name}")
    } else {
        parts.join("::")
    };
    super::emit_message(cx, span, format!(
        "`{}` bypasses bounded process ownership; use a daemon process facade with a mandatory ProcessLiveness policy; raw sessions belong only in process_session.rs functions with an explicit finite budget",
        display_path
    ));
    true
}

fn enclosing_function_has_budget(cx: &LateContext<'_>) -> bool {
    let Some(body) = cx.enclosing_body else {
        return false;
    };
    let mut owner = cx.tcx.hir_body_owner_def_id(body).to_def_id();
    loop {
        if matches!(cx.tcx.def_kind(owner), DefKind::Fn | DefKind::AssocFn) {
            return cx
                .tcx
                .fn_sig(owner)
                .instantiate_identity()
                .skip_binder()
                .inputs()
                .iter()
                .any(|input| {
                    let ty::Adt(adt, _) = input.peel_refs().kind() else {
                        return false;
                    };
                    let path = cx.tcx.def_path_str(adt.did());
                    matches!(
                        path.as_str(),
                        "core::time::Duration" | "std::time::Duration"
                    ) || (cx.tcx.crate_name(adt.did().krate).as_str() == "zccache_daemon_core"
                        && super::path_ends_with(
                            &cx.get_def_path(adt.did()),
                            &["daemon", "process", "policy", "ProcessLiveness"],
                        ))
                });
        }
        let Some(parent) = cx.tcx.opt_parent(owner) else {
            return false;
        };
        owner = parent;
    }
}
