#![allow(dead_code)]
use zccache_artifact::DetachedKvNamespace;
use zccache_core::sync::PublicationReadPermit;
struct Cleanup(Option<DetachedKvNamespace>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(retired) = self.0.take() {
            let _ = retired.sweep_for(std::time::Duration::from_secs(1));
        }
    }
}
fn pointer(permit: PublicationReadPermit, cleanup: Cleanup) {
    let mut value = std::mem::ManuallyDrop::new(cleanup);
    unsafe {
        std::ptr::drop_in_place(&mut *value);
    }
    drop(permit);
}
fn manual(permit: PublicationReadPermit, cleanup: Cleanup) {
    let mut value = std::mem::ManuallyDrop::new(cleanup);
    unsafe {
        std::mem::ManuallyDrop::drop(&mut value);
    }
    drop(permit);
}
fn uninitialized(permit: PublicationReadPermit, cleanup: Cleanup) {
    let mut value = std::mem::MaybeUninit::new(cleanup);
    unsafe {
        value.assume_init_drop();
    }
    drop(permit);
}
fn released(permit: PublicationReadPermit, cleanup: Cleanup) {
    let mut value = std::mem::ManuallyDrop::new(cleanup);
    drop(permit);
    unsafe {
        std::mem::ManuallyDrop::drop(&mut value);
    }
}
fn alias(permit: PublicationReadPermit, cleanup: Cleanup) {
    use std::ptr::drop_in_place as destroy;
    let mut value = std::mem::ManuallyDrop::new(cleanup);
    unsafe {
        destroy(&mut *value);
    }
    drop(permit);
}
fn item(permit: PublicationReadPermit, cleanup: Cleanup) {
    let destroy = std::mem::MaybeUninit::<Cleanup>::assume_init_drop;
    let mut value = std::mem::MaybeUninit::new(cleanup);
    unsafe {
        destroy(&mut value);
    }
    drop(permit);
}
fn released_pointer(permit: PublicationReadPermit, cleanup: Cleanup) {
    let mut value = std::mem::ManuallyDrop::new(cleanup);
    drop(permit);
    unsafe {
        std::ptr::drop_in_place(&mut *value);
    }
}
fn released_uninitialized(permit: PublicationReadPermit, cleanup: Cleanup) {
    let mut value = std::mem::MaybeUninit::new(cleanup);
    drop(permit);
    unsafe {
        value.assume_init_drop();
    }
}
fn main() {}
