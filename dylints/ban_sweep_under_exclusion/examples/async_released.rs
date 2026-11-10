#![allow(dead_code)]
use zccache_core::sync::PublicationReadPermit;
async fn released<T>(permit: PublicationReadPermit, operation: &mut Option<T>) {
    std::future::ready(()).await;
    drop(permit);
    drop(operation.take());
}
async fn successive<T>(permit: PublicationReadPermit, operation: &mut Option<T>) {
    std::future::ready(()).await;
    drop(permit);
    std::future::ready(()).await;
    drop(operation.take());
}
fn main() {}
