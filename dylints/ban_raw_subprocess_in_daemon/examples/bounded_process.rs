#![allow(dead_code)]
use std::time::Duration;
fn bounded(tool: kernal_api::SpawnSpec) {
    let _ = zccache_core::wait::output_for(tool, Duration::from_secs(5));
}
struct Other;
impl Other {
    async fn wait(&self) {}
    async fn spawn_session(&self) {}
}
async fn unrelated(other: Other) {
    other.wait().await;
    other.spawn_session().await;
}
fn main() {}
