#![allow(dead_code)]
use std::time::Duration;
use zccache_artifact::DetachedKvNamespace;
use zccache_core::sync::PublicationCommitPermit;
fn cleanup(retired: DetachedKvNamespace) {
    let _ = retired.sweep_for(Duration::from_secs(1));
}
fn held(guard: PublicationCommitPermit, retired: DetachedKvNamespace) {
    cleanup(retired);
    drop(guard);
}
fn main() {}
