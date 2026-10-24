#![allow(dead_code, dropping_references)]
use zccache_core::sync::PublicationCommitPermit;
fn borrowed(permit: &mut PublicationCommitPermit) {
    drop(permit);
    let _ = std::fs::read_dir("owned-cache");
}
struct Borrowed<'a> {
    guard: &'a mut PublicationCommitPermit,
}
fn wrapped(permit: Borrowed<'_>) {
    drop(permit);
    let _ = std::fs::read_dir("owned-cache");
}
fn main() {}
