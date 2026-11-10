#![allow(dead_code)]
struct CachedArtifact;
impl CachedArtifact {
    fn from_artifact_data(_: &()) -> Self { Self }
    fn from_artifact_data_with_modes(_: &(), _: Vec<u32>) -> Self { Self }
}
type Alias = CachedArtifact;
fn main() {
    let _ = CachedArtifact::from_artifact_data(&());
    let _ = Alias::from_artifact_data(&());
    let _ = <Alias>::from_artifact_data_with_modes(&(), Vec::new());
    let constructor = CachedArtifact::from_artifact_data;
    let _ = constructor(&());
}
