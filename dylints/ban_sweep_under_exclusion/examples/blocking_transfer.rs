#![allow(dead_code)]
use zccache_core::sync::PublicationReadPermit;
fn transfer<T: Send + 'static>(permit: PublicationReadPermit, value: T) {
    kernal_api::async_engine::launch_blocking(move || {
        drop(permit);
        drop(value);
    })
    .detach();
}
fn binding_order<T: Send + 'static>(permit: PublicationReadPermit, value: T) {
    kernal_api::async_engine::launch_blocking(move || {
        let _ = &permit;
        let _empty = Vec::<u8>::new();
        let _value = value;
        let _permit = permit;
    })
    .detach();
}
fn main() {}
