//! Compiler-only access to the real private payload/owner lifetime contract.
//! No alternate lease algorithm: the wrapper contains MaterializationPayloads.
use super::*;

/// Inline cached record used by external compiler contract fixtures.
#[doc(hidden)]
pub struct TestMaterializationOwner {
    cached: CachedArtifact,
}

/// Wrapper around the actual delivery lease, with no invented lifetime marker.
#[doc(hidden)]
pub struct TestMaterializationLease<'a> {
    inner: MaterializationPayloads<'a>,
}

impl TestMaterializationOwner {
    /// Build a filesystem-free record; runtime filesystem behavior has separate tests.
    pub fn inline() -> Self {
        let cached = CachedArtifact::from_index(ArtifactIndex::new(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            0,
        ));
        // The empty artifact has no resident bytes or disk payloads. Initialize
        // its real delivery cell without bypassing the provisional-owner API.
        cached.payloads.get_or_init(|| Arc::from([]));
        Self { cached }
    }

    /// Expose the actual payload type's ownership contract to the compiler.
    #[expect(
        clippy::unwrap_used,
        reason = "inline fixture always initializes payload storage"
    )]
    pub fn lease(&self) -> TestMaterializationLease<'_> {
        TestMaterializationLease {
            inner: MaterializationPayloads {
                payloads: Arc::clone(self.cached.payloads.get().unwrap()),
                staged_guard: None,
                cached: &self.cached,
            },
        }
    }
}

impl TestMaterializationLease<'_> {
    /// Observe a live lease after a candidate owner drop.
    pub fn payload_count(&self) -> usize {
        self.inner.len()
    }
}
