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
fn checked(value: usize) {
    assert!(value == 0);
}
struct Reversed {
    cleanup: Cleanup,
    permit: PublicationReadPermit,
}
fn reversed(permit: PublicationReadPermit, cleanup: Cleanup, value: usize) {
    let _owner = Reversed { cleanup, permit };
    checked(value);
}
struct Ordered {
    permit: PublicationReadPermit,
    cleanup: Cleanup,
}
fn borrowed(
    outside: &PublicationReadPermit,
    permit: PublicationReadPermit,
    cleanup: Cleanup,
    value: usize,
) {
    let _owner = Ordered { permit, cleanup };
    checked(value);
    let _ = outside;
}
struct Custom {
    permit: PublicationReadPermit,
    cleanup: Cleanup,
}
impl Drop for Custom {
    fn drop(&mut self) {
        let old = std::mem::replace(&mut self.cleanup, Cleanup(None));
        drop(old);
    }
}
fn custom(permit: PublicationReadPermit, cleanup: Cleanup, value: usize) {
    let _owner = Custom { permit, cleanup };
    checked(value);
}
fn explicit_reversed(permit: PublicationReadPermit, cleanup: Cleanup) {
    drop(Reversed { cleanup, permit });
}
fn explicit_borrowed(
    outside: &PublicationReadPermit,
    permit: PublicationReadPermit,
    cleanup: Cleanup,
) {
    drop(Ordered { permit, cleanup });
    let _ = outside;
}
fn main() {}
