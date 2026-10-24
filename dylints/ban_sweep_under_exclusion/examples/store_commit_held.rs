#![allow(dead_code)]
use zccache_artifact::StagedCommitGuard as Commit;
fn blocked(guard: Commit) {
    let _ = std::fs::read_dir("owned-cache");
    drop(guard);
}
fn main() {}
