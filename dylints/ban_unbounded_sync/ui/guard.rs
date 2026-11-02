// compile-flags: --edition=2021
#![allow(dead_code)]
use std::sync::MutexGuard as Renamed;
async fn hidden_factory_guard(guard: Renamed<'_, u32>) {
    let nested = Some(guard);
    std::future::ready(()).await;
    drop(nested);
}
struct Token { guard: std::sync::MutexGuard<'static, u32> }
async fn concrete_wrapper(token: Token) {
    std::future::ready(()).await;
    drop(token);
}
fn main() {}
