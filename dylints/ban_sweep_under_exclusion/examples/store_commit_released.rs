#![allow(dead_code)]
use zccache_artifact::{StagedCommitGuard, StagedReadGuard};
fn released(guard: StagedCommitGuard) {
    drop(guard);
    let _ = std::fs::read_dir("owned-cache");
}
fn retained_reader(guard: StagedReadGuard) {
    let _ = std::fs::read_dir("owned-cache");
    drop(guard);
}
fn main() {}
