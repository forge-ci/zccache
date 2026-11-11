// compile-flags: --edition=2021
#![allow(dead_code)]
use std::sync::RwLockWriteGuard as Guard;
trait MemoCycleA {}
trait MemoCycleB {}
struct SlowA(Option<Box<dyn MemoCycleB>>, Option<Box<dyn MemoCycleB>>);
struct NodeB(Option<Box<dyn MemoCycleA>>);
impl MemoCycleA for SlowA {}
impl MemoCycleB for NodeB {}
impl Drop for SlowA {
    fn drop(&mut self) {
        let _ = std::fs::read_dir("owned-cache");
    }
}
fn sources(a: SlowA, b: NodeB) -> (Box<dyn MemoCycleA>, Box<dyn MemoCycleB>) {
    (Box::new(a), Box::new(b))
}
fn roots(guard: Guard<'_, ()>, first: Box<dyn MemoCycleA>, second: Box<dyn MemoCycleB>) {
    drop(first);
    drop(second);
    drop(guard);
}
fn main() {}
