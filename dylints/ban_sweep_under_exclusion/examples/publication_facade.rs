#![allow(dead_code)]
use std::time::Duration;
use zccache_core::sync::{PublicationBarrier, PublicationCommitPermit, PublicationDrainPermit};
fn commit(barrier: &PublicationBarrier) {
    let permit: PublicationCommitPermit = barrier.blocking_write_for(Duration::from_secs(1));
    drop(permit);
}
fn drain_kind(_: PublicationDrainPermit) {}
fn main() {}
