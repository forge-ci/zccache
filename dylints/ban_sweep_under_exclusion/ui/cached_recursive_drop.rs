// compile-flags: --edition=2021
#![allow(dead_code)]
use std::sync::RwLockWriteGuard as Guard;
struct CycleCleanup;
impl Drop for CycleCleanup {
    fn drop(&mut self) {
        let _ = std::fs::read_dir("owned-cache");
    }
}
enum First { Next(Box<Second>), Cleanup(CycleCleanup) }
enum Second { Next(Box<First>), Empty }
fn roots(guard: Guard<'_, ()>, first: First, second: Second) {
    drop(first);
    drop(second);
    drop(guard);
}
fn main() {}
