//! zccache CI-specific process-tree termination policy.
//!
//! Kernal-api owns the native Unix session/process-group capability, proven
//! through the owner's unreaped `Child`. Windows has no numeric group kill and
//! reports `Unsupported`, so the CI runner keeps `taskkill /T` there as its
//! retained tree-kill fallback.

const TREE_KILL_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

pub(crate) fn force_process_group(child: &std::process::Child) {
    match kernal_api::platform::process::force_terminate_process_group(child) {
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
            // A failed tree kill is evidence for the hang being cleaned up, so
            // keep it (#1970). The result is otherwise unused: the caller reaps
            // the direct child next, and the bound keeps a stuck taskkill from
            // stalling that path.
            let spec = kernal_api::SpawnSpec::new("taskkill").args([
                "/T",
                "/F",
                "/PID",
                &child.id().to_string(),
            ]);
            drop(zccache_core::small_tool::output_for_recorded(
                "taskkill",
                spec,
                TREE_KILL_BOUND,
            ));
        }
        // Best-effort: the caller is already on the failure path and reaps
        // the direct child next.
        _ => {}
    }
}
