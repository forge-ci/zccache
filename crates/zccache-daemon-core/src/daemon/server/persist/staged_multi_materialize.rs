//! Requested-path delivery for the owned staging plan.
use super::*;

impl StagedMultiUnitPlan {
    pub(in crate::daemon::server) fn materialize(
        &self,
        mode: MaterializationMode,
    ) -> std::io::Result<StagedMaterializationStats> {
        let mut observed = StagedMaterializationStats::default();
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
                .map_err(|error| materialization_error(error, observed))?;
            }
            if let Some(parent) = output.requested.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| materialization_error(error, observed))?;
            }
            let output_stats = materialize_independent_with_mode(
                output.staged.as_path(),
                output.requested.as_path(),
                mode,
            )
            .map_err(|error| materialization_error(error, observed))?;
            observed.reflink_count = observed
                .reflink_count
                .saturating_add(output_stats.reflink_count);
            observed.copy_count = observed.copy_count.saturating_add(output_stats.copy_count);
            observed.copy_bytes = observed.copy_bytes.saturating_add(output_stats.copy_bytes);
            if output.role == StagedOutputRole::Depfile {
                rehydrate_logical_depfile(output.requested.as_path(), &requested_outputs)
                    .map_err(|error| materialization_error(error, observed))?;
            }
        }
        Ok(observed)
    }
}
