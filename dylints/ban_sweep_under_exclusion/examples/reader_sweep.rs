#![allow(dead_code)]
use std::time::Duration;
use zccache_artifact::DetachedKvNamespace;
use zccache_core::sync::{PublicationDrainPermit, PublicationReadPermit};
fn cleanup(retired: DetachedKvNamespace) {
    let _ = retired.sweep_for(Duration::from_secs(1));
}
fn reader(permit: PublicationReadPermit, retired: DetachedKvNamespace) {
    cleanup(retired);
    drop(permit);
}
fn drain(permit: PublicationDrainPermit, retired: DetachedKvNamespace) {
    cleanup(retired);
    drop(permit);
}
fn released(permit: PublicationReadPermit, retired: DetachedKvNamespace) {
    drop(permit);
    cleanup(retired);
}
fn main() {}
