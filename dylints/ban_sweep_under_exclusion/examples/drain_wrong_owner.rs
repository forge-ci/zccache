#![allow(dead_code)]
use std::time::Duration;
use zccache_core::sync::PublicationBarrier as Coordinator;
async fn wrong_owner(barrier: &Coordinator) {
    let permit = barrier.drain_for(Duration::from_secs(1)).await;
    drop(permit);
}
fn function_item() {
    let _drain = Coordinator::drain_for;
}
async fn ufcs(barrier: &Coordinator) {
    let permit = Coordinator::drain_for(barrier, Duration::from_secs(1)).await;
    drop(permit);
}
fn main() {}
