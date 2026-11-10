#![allow(dead_code)]
use zccache_core::sync::PublicationReadPermit;
fn forget_value<T>(value: T) {
    std::mem::forget(value);
}
fn held(permit: PublicationReadPermit) {
    forget_value(permit);
}
fn non_owning(permit: PublicationReadPermit) {
    forget_value(std::marker::PhantomData::<PublicationReadPermit>);
    let _erased = erase_value(std::marker::PhantomData::<PublicationReadPermit>);
    drop(permit);
}
fn erase_value<T: 'static>(value: T) -> Box<dyn std::any::Any> {
    Box::new(value)
}
fn erased(permit: PublicationReadPermit) {
    let _erased = erase_value(permit);
}
fn main() {}
