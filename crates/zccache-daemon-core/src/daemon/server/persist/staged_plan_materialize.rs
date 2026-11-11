//! Requested-path delivery for the owned staging plan.
use super::super::{materialize_independent_with_mode_scoped, OutputScope, SharedState};
use super::*;

impl StagedCompilePlan {
    pub(in crate::daemon::server) fn materialize_scoped(
        &self,
        scope: &OutputScope,
        mode: MaterializationMode,
    ) -> io::Result<StagedMaterializationStats> {
        let stats = self.materialize_without_cleanup_scoped(scope, mode)?;
        self.cleanup()
            .map_err(|error| materialization_error(error, stats))?;
        Ok(stats)
    }

    /// Materialize the requested outputs while retaining the private staging
    /// root for a detached durable publisher. The caller must keep this plan
    /// alive until publication has consumed every staged source.
    pub(in crate::daemon::server) fn materialize_without_cleanup_scoped(
        &self,
        scope: &OutputScope,
        mode: MaterializationMode,
    ) -> io::Result<StagedMaterializationStats> {
        let mut stats = StagedMaterializationStats::default();
        let requested_outputs = self
            .outputs
            .iter()
            .map(|output| output.requested.clone())
            .collect::<Vec<_>>();
        for (fault_index, output) in self.outputs.iter().enumerate() {
            #[cfg(not(test))]
            let _ = fault_index;
            #[cfg(test)]
            {
                inject_staged_fault(
                    output.requested.as_path(),
                    StagedFaultPoint::MaterializeOutput(fault_index),
                )
                .map_err(|error| materialization_error(error, stats))?;
            }
            let destination = scope
                .output(output.requested.as_path())
                .map_err(|error| materialization_error(error, stats))?;
            if let Some(parent) = output.requested.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| materialization_error(error, stats))?;
            }
            let output_stats = materialize_independent_with_mode_scoped(
                output.staged.as_path(),
                &destination,
                mode,
            )
            .map_err(|error| {
                materialization_error(
                    io::Error::new(
                        error.kind(),
                        format!(
                            "{} -> {}: {error}",
                            output.staged.display(),
                            output.requested.display()
                        ),
                    ),
                    stats,
                )
            })?;
            stats.reflink_count = stats
                .reflink_count
                .saturating_add(output_stats.reflink_count);
            stats.copy_count = stats.copy_count.saturating_add(output_stats.copy_count);
            stats.copy_bytes = stats.copy_bytes.saturating_add(output_stats.copy_bytes);
            if output.role == StagedOutputRole::Depfile {
                rehydrate_logical_depfile_scoped(&destination, &requested_outputs)
                    .map_err(|error| materialization_error(error, stats))?;
            }
        }
        Ok(stats)
    }
    pub(in crate::daemon::server) fn materialize_for_state(
        &self,
        state: &SharedState,
        mode: MaterializationMode,
    ) -> std::io::Result<StagedMaterializationStats> {
        self.materialize_scoped(&OutputScope::for_state(state)?, mode)
    }
    #[cfg(test)]
    pub(in crate::daemon::server) fn materialize(
        &self,
        mode: MaterializationMode,
    ) -> std::io::Result<StagedMaterializationStats> {
        self.materialize_scoped(&OutputScope::unrestricted_for_test(), mode)
    }

    pub(in crate::daemon::server) fn materialize_without_cleanup_for_state(
        &self,
        state: &SharedState,
        mode: MaterializationMode,
    ) -> std::io::Result<StagedMaterializationStats> {
        self.materialize_without_cleanup_scoped(&OutputScope::for_state(state)?, mode)
    }
    #[cfg(test)]
    pub(in crate::daemon::server) fn materialize_without_cleanup(
        &self,
        mode: MaterializationMode,
    ) -> std::io::Result<StagedMaterializationStats> {
        self.materialize_without_cleanup_scoped(&OutputScope::unrestricted_for_test(), mode)
    }
}
