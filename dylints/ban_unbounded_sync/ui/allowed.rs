// compile-flags: --edition=2021
#![allow(dead_code)]
struct Other;
impl Other { fn lock(&self) {} fn recv(&self) {} }
async fn released_before_await(guard: std::sync::MutexGuard<'_, u32>) {
    { let guard = guard; drop(guard); }
    std::future::ready(()).await;
}
fn main() { Other.lock(); Other.recv(); }
fn receive(timeout: std::time::Duration) { let _ = timeout; }
fn observed_elapsed(elapsed: Option<std::time::Duration>) { let _ = elapsed; }
fn optional_identifier(id: Option<u64>) { let _ = id; }
fn wait_for_identity(pid: Option<u32>) { let _ = pid; }
