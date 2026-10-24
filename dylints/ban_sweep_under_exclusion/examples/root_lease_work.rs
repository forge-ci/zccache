#![allow(dead_code)]
use zccache_artifact::DetachedKvNamespace;
use zccache_core::cache_root_lock::CacheRootWriterLease;
fn blocking_cleanup(lease: CacheRootWriterLease, retired: DetachedKvNamespace) {
    let _ = retired.sweep_for(std::time::Duration::from_secs(1));
    drop(lease);
}
async fn operation_lifetime(lease: CacheRootWriterLease) {
    std::future::ready(()).await;
    drop(lease);
}
fn forget_value<T>(value: T) {
    std::mem::forget(value);
}
fn non_owning() {
    forget_value(std::marker::PhantomData::<CacheRootWriterLease>);
}
fn main() {}
fn shared_cleanup(lease: std::sync::Arc<CacheRootWriterLease>, retired: DetachedKvNamespace) {
    let _ = retired.sweep_for(std::time::Duration::from_secs(1));
    drop(lease);
}
fn weak_handle(lease: std::sync::Weak<CacheRootWriterLease>) {
    forget_value(lease);
}
async fn shared_blocking_job(
    lease: std::sync::Arc<CacheRootWriterLease>,
    retired: DetachedKvNamespace,
) {
    let job_root = std::sync::Arc::clone(&lease);
    let job = kernal_api::async_engine::launch_blocking(move || {
        let _root = job_root;
        let _ = retired.sweep_for(std::time::Duration::from_secs(1));
    })
    .detach_on_drop();
    let _ = job.await;
    drop(lease);
}
