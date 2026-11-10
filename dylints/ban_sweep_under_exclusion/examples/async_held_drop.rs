#![allow(dead_code)]
use zccache_core::sync::PublicationReadPermit;
async fn held<T>(permit: PublicationReadPermit, operation: &mut Option<T>) {
    std::future::ready(()).await;
    drop(operation.take());
    drop(permit);
}
async fn held_twice<T>(permit: PublicationReadPermit, operation: &mut Option<T>) {
    std::future::ready(()).await;
    std::future::ready(()).await;
    drop(operation.take());
    drop(permit);
}
fn main() {}
