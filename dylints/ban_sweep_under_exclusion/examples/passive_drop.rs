#![allow(dead_code, dropping_copy_types, undropped_manually_drops)]
use std::{
    marker::PhantomData,
    mem::{ManuallyDrop, MaybeUninit},
    sync::Weak,
    time::Duration,
};
use zccache_artifact::DetachedKvNamespace;
use zccache_core::sync::PublicationReadPermit;
struct Cleanup(Option<DetachedKvNamespace>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(retired) = self.0.take() {
            let _ = retired.sweep_for(Duration::from_secs(1));
        }
    }
}
fn passive(
    permit: PublicationReadPermit,
    weak: Weak<Cleanup>,
    phantom: PhantomData<Cleanup>,
    manual: ManuallyDrop<Cleanup>,
    uninit: MaybeUninit<Cleanup>,
) {
    drop(weak);
    drop(phantom);
    drop(manual);
    drop(uninit);
    drop(permit);
}
fn main() {}
