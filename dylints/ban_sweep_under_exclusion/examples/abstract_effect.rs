#![allow(dead_code)]
use zccache_core::sync::PublicationCommitPermit;
trait Cleanup {
    fn cleanup(&self);
}
fn held(permit: PublicationCommitPermit, operation: &impl Cleanup) {
    operation.cleanup();
    drop(permit);
}
fn main() {}
