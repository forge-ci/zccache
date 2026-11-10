#![allow(dead_code)]
use std::time::Duration;
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
fn held(permit: PublicationReadPermit, cleanup: Cleanup) {
    let callback = async move { drop(cleanup) };
    drop(callback);
    drop(permit);
}
fn released(permit: PublicationReadPermit, cleanup: Cleanup) {
    let callback = async move { drop(cleanup) };
    drop(permit);
    drop(callback);
}
async fn saved_cleanup(make: fn() -> Cleanup) {
    let cleanup = make();
    std::future::pending::<()>().await;
    drop(cleanup);
}
fn held_saved(permit: PublicationReadPermit, make: fn() -> Cleanup) {
    // May be cancelled before polling or after entering the pending state.
    let callback = saved_cleanup(make);
    drop(callback);
    drop(permit);
}
fn released_saved(permit: PublicationReadPermit, make: fn() -> Cleanup) {
    let callback = saved_cleanup(make);
    drop(permit);
    drop(callback);
}
struct CustomFuture(Cleanup);
impl std::future::Future for CustomFuture {
    type Output = ();
    fn poll(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        std::task::Poll::Ready(())
    }
}
async fn held_custom(permit: PublicationReadPermit, cleanup: Cleanup) {
    CustomFuture(cleanup).await;
    drop(permit);
}
fn main() {}
