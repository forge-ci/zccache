#![allow(dead_code)]
use std::time::Duration;
use zccache_artifact::DetachedKvNamespace;
use zccache_core::sync::PublicationReadPermit;
fn invoke<F: FnOnce()>(operation: F) {
    operation();
}
fn held(permit: PublicationReadPermit, retired: DetachedKvNamespace) {
    invoke(|| {
        let _ = retired.sweep_for(Duration::from_secs(1));
    });
    drop(permit);
}
fn main() {}
