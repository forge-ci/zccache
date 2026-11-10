//! Own resident payloads only while durable publication is incomplete (#1965).

use super::*;

/// The only production boundary for constructing and publishing resident entries.
/// Keeping the cached entry private prevents callers from inserting its bytes as
/// an already-durable row. An in-progress hit may retain its own owned clone.
#[derive(Clone)]
pub(in crate::daemon::server) struct ProvisionalArtifact {
    cached: CachedArtifact,
}

impl ProvisionalArtifact {
    pub(in crate::daemon::server) fn new(artifact: &ArtifactData, modes: Vec<u32>) -> Self {
        Self {
            cached: CachedArtifact::from_artifact_data_with_modes(artifact, modes),
        }
    }

    pub(in crate::daemon::server) fn meta(&self) -> &ArtifactIndex {
        &self.cached.meta
    }

    /// Publish immediate hits while persistence owns the payload bytes.
    pub(in crate::daemon::server) fn publish(
        &self,
        artifacts: &DashMap<String, CachedArtifact>,
        key: String,
    ) {
        artifacts.insert(key, self.cached.clone());
    }

    /// Call only after successful payload/index publication. Payload discovery
    /// follows the existing lazy disk-backed hit path, without extra hit I/O.
    pub(in crate::daemon::server) fn durable(&self) -> CachedArtifact {
        let durable = CachedArtifact::from_index(self.cached.meta.clone());
        *durable.access() = self.cached.access().clone();
        durable
    }

    /// A stale completion must not replace a newer same-key publication. The
    /// comparison and replacement share one map guard; no filesystem I/O occurs
    /// while it is held. Failed persistence never calls this operation.
    pub(in crate::daemon::server) fn demote_to_durable(
        &self,
        artifacts: &DashMap<String, CachedArtifact>,
        key: &str,
    ) {
        if let Some(mut current) = artifacts.get_mut(key) {
            if current.same_instance(&self.cached) {
                *current = self.durable();
            }
        }
    }
}
