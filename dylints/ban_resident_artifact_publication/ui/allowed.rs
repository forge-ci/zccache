#![allow(dead_code)]
mod cached_artifact {
    pub struct CachedArtifact;
    impl CachedArtifact {
        fn from_artifact_data(_: &()) -> Self { Self }
        pub fn from_index(_: &()) -> Self { Self }
    }
    pub mod provisional {
        pub fn publish() {
            let _ = super::CachedArtifact::from_artifact_data(&());
        }
    }
}
fn main() {
    cached_artifact::provisional::publish();
    let _ = cached_artifact::CachedArtifact::from_index(&());
}
