//! Exact function/type identities for audited tree cleanup owners.
use rustc_lint::LateContext;
use rustc_middle::ty;

pub(super) fn approved(cx: &LateContext<'_>, filename: &str) -> bool {
    let Some(owner) = super::enclosing_function(cx) else {
        return false;
    };
    let path = super::canonical_path(cx, owner);
    const FREE: &[(&str, &str)] = &[
        ("zccache_artifact::kv::retirement::sweep_tree", "crates/zccache-artifact/src/kv/retirement.rs"),
        ("zccache_daemon_core::daemon::server::persist::staged_store::trash::remove_tree", "/persist/staged_store/trash.rs"),
        ("zccache_daemon_core::daemon::server::persist::staged_store::trash::try_remove_empty_trash", "/persist/staged_store/trash.rs"),
        ("zccache_daemon_core::daemon::server::persist::staged_store::root_safety::remove_staged_link_or_reparse", "/persist/staged_store/root_safety.rs"),
        ("zccache_daemon_core::daemon::server::persist::directory_bundle::remove_directory_if_present", "/persist/directory_bundle.rs"),
        ("zccache_daemon_core::daemon::server::staging_root::sweep_private_trash", "/server/staging_root.rs"),
        ("zccache_daemon_core::daemon::server::staging_root::remove_retired_root", "/server/staging_root.rs"),
    ];
    if FREE
        .iter()
        .any(|(function, file)| path == *function && filename.ends_with(file))
    {
        return true;
    }
    let Some(implementation) = cx.tcx.impl_of_assoc(owner) else {
        return false;
    };
    let value = cx
        .tcx
        .type_of(implementation)
        .instantiate_identity()
        .skip_norm_wip();
    let ty::Adt(definition, _) = value.kind() else {
        return false;
    };
    let receiver = super::canonical_path(cx, definition.did());
    let method = cx.tcx.item_name(owner);
    const METHODS: &[(&str, &str, &[&str])] = &[
        (
            "zccache_daemon_core::daemon::server::staging_root::StagingRoot",
            "/server/staging_root.rs",
            &["cleanup_abandoned_older_than", "drop"],
        ),
        (
            "zccache_daemon_core::daemon::server::handle_exec::ExecStagedPlan",
            "/server/handle_exec.rs",
            &["build", "cleanup"],
        ),
        (
            "zccache_daemon_core::daemon::server::persist::staged_plan::StagedCompilePlan",
            "/persist/staged_plan.rs",
            &["rustc", "cc", "archive", "link", "cleanup"],
        ),
        (
            "zccache_daemon_core::daemon::server::persist::staged_multi::StagedMultiUnitPlan",
            "/persist/staged_multi.rs",
            &["build_with_depfile_flag", "cleanup"],
        ),
        (
            "zccache_daemon_core::daemon::server::DaemonServer",
            "/server/run.rs",
            &["run"],
        ),
        (
            "zccache_daemon_core::daemon::server::EmbeddedDaemon",
            "/server/embedded.rs",
            &["shutdown"],
        ),
    ];
    METHODS.iter().any(|(receiver_path, file, methods)| {
        receiver == *receiver_path && filename.ends_with(file) && methods.contains(&method.as_str())
    })
}
