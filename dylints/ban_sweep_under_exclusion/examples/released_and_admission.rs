#![allow(dead_code)]
use zccache_core::sync::{PublicationCommitPermit as Commit, PublicationDrainPermit, WritePermit};
fn released(permit: Commit) {
    drop(permit);
    let _ = std::fs::read_dir("owned-cache");
}
fn checkpoint(permit: PublicationDrainPermit) {
    let _ = std::fs::read_dir("owned-cache");
    drop(permit);
}
fn admission(permit: WritePermit) {
    let _ = std::fs::read_dir("owned-cache");
    drop(permit);
}
fn main() {}
