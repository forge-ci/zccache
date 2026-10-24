#![allow(dead_code, dropping_references)]
use std::time::Duration;
use zccache_artifact::DetachedKvNamespace;
use zccache_core::sync::PublicationReadPermit;
async fn borrowed(permit: &mut PublicationReadPermit, retired: DetachedKvNamespace) {
    std::future::ready(()).await;
    drop(permit);
    let _ = retired.sweep_for(Duration::from_secs(1));
}
fn main() {}
