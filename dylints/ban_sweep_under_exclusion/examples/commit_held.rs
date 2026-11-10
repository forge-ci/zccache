#![allow(dead_code)]
use zccache_core::sync::PublicationCommitPermit as Commit;
fn blocked(permit: Commit) {
    let _ = std::fs::read_dir("owned-cache");
    drop(permit);
}
fn main() {}
