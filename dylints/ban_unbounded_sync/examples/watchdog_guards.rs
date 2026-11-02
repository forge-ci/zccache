#![allow(dead_code)]
use zccache_core::blocking_sync::Mutex as Bounded;
use zccache_core::blocking_sync::{MutexGuard as Alias, RwLockReadGuard, RwLockWriteGuard};
async fn aliased_guard(guard: Alias<'_, u32>) {
    let nested = Some(guard);
    std::future::ready(()).await;
    drop(nested);
}
struct Wrapper {
    guard: RwLockReadGuard<'static, u32>,
}
async fn concrete_wrapper(token: Wrapper) {
    std::future::ready(()).await;
    drop(token);
}
async fn native_unit_guard(guard: RwLockWriteGuard<'_, ()>) {
    std::future::ready(()).await;
    drop(guard);
}
async fn blocking_bypasses(lock: &Bounded<u32>) {
    let _function_item = Bounded::<u32>::lock_for;
    let _guard = Bounded::lock_for(lock, std::time::Duration::from_secs(1));
}
fn main() {}
