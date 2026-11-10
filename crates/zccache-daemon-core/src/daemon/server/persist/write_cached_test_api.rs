//! Raw-path adapters for existing unit fixtures; production requires authority.
use super::*;
use std::io;
fn destinations<P: AsRef<Path>>(paths: &[P]) -> Vec<OutputDestination> {
    paths
        .iter()
        .map(|path| OutputDestination::for_test(path.as_ref()))
        .collect()
}
pub(in crate::daemon::server) fn write_payloads_par_observed<P: AsRef<Path> + Sync>(
    targets: &[P],
    payloads: &[CachedPayload],
    mode: MaterializationMode,
) -> MaterializationResult<StagedMaterializationStats> {
    write_payloads_par_observed_scoped(&destinations(targets), payloads, mode)
}
pub(in crate::daemon::server) fn write_payloads_par_with_delivery<P: AsRef<Path> + Sync>(
    targets: &[P],
    payloads: &[CachedPayload],
    mode: MaterializationMode,
    delivery: impl Fn(&Path) -> crate::compiler::DeliveryPolicy + Sync,
) -> MaterializationResult<StagedMaterializationStats> {
    write_payloads_par_with_delivery_scoped(&destinations(targets), payloads, mode, delivery)
}

pub(in crate::daemon::server) fn write_payloads_par_with_mtime_floor_and_policies_observed<
    P: AsRef<Path> + Sync,
    R: AsRef<Path>,
>(
    targets: &[P],
    payloads: &[CachedPayload],
    floors: &[R],
    policies: &[crate::compiler::DeliveryPolicy],
    mode: MaterializationMode,
) -> MaterializationResult<StagedMaterializationStats> {
    write_payloads_par_with_mtime_floor_and_policies_observed_scoped(
        &destinations(targets),
        payloads,
        floors,
        policies,
        mode,
    )
}

pub(in crate::daemon::server) fn materialize_verified_cached_file_observed(
    out: &Path,
    cache: &Path,
    delivery: crate::compiler::DeliveryPolicy,
    mode: MaterializationMode,
    force: bool,
    pass: SiblingFloorPass,
) -> io::Result<StagedMaterializationStats> {
    materialize_verified_cached_file_observed_scoped(
        &OutputDestination::for_test(out),
        cache,
        delivery,
        mode,
        force,
        pass,
    )
}
