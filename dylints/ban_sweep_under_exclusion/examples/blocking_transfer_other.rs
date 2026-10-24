#![allow(dead_code)]
use zccache_core::sync::PublicationReadPermit;
fn retained<T: Send + 'static>(
    first: PublicationReadPermit,
    second: PublicationReadPermit,
    value: T,
) {
    kernal_api::async_engine::launch_blocking(move || {
        drop(first);
        drop(value);
    })
    .detach();
    drop(second);
}
fn wrong_capture_order<T: Send + 'static>(permit: PublicationReadPermit, value: T) {
    kernal_api::async_engine::launch_blocking(move || {
        let value = value;
        drop(permit);
        drop(value);
    })
    .detach();
}
fn wrong_body<T: Send + 'static>(permit: PublicationReadPermit, value: T) {
    kernal_api::async_engine::launch_blocking(move || {
        let held = permit;
        drop(value);
        drop(held);
    })
    .detach();
}
fn shared_owner<T: Send + 'static>(permit: std::sync::Arc<PublicationReadPermit>, value: T) {
    kernal_api::async_engine::launch_blocking(move || {
        drop(permit);
        drop(value);
    })
    .detach();
}
fn main() {}
