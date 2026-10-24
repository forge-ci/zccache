#![allow(dead_code)]
use std::time::Duration;
use zccache_artifact::DetachedKvNamespace;
use zccache_core::sync::PublicationReadPermit;
fn install(slot: &mut Option<PublicationReadPermit>, permit: PublicationReadPermit) {
    *slot = Some(permit);
}
fn held(
    permit: PublicationReadPermit,
    retired: DetachedKvNamespace,
    mut slot: Option<PublicationReadPermit>,
) {
    match slot {
        None => install(&mut slot, permit),
        Some(_) => unreachable!(),
    }
    let _ = retired.sweep_for(Duration::from_secs(1));
    drop(slot);
}
fn main() {}
