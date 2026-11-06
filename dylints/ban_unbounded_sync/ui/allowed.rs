// compile-flags: --edition=2021
#![allow(dead_code)]
struct Other;
impl Other { fn lock(&self) {} fn recv(&self) {} }
async fn released_before_await(guard: std::sync::MutexGuard<'_, u32>) {
    { let guard = guard; drop(guard); }
    std::future::ready(()).await;
}
fn main() { Other.lock(); Other.recv(); }
