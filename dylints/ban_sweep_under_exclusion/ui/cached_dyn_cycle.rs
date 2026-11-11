// compile-flags: --edition=2021
#![allow(dead_code)]
use std::sync::RwLockWriteGuard as Guard;
trait A {}
trait B {}
struct SlowA(Option<Box<dyn B>>);
struct NodeB(Option<Box<dyn A>>);
impl A for SlowA {}
impl B for NodeB {}
impl Drop for SlowA {
    fn drop(&mut self) {
        let _ = std::fs::read_dir("owned-cache");
    }
}
fn sources(a: SlowA, b: NodeB) -> (Box<dyn A>, Box<dyn B>) {
    (Box::new(a), Box::new(b))
}
fn roots(guard: Guard<'_, ()>, first: Box<dyn A>, second: Box<dyn B>) {
    drop(first);
    drop(second);
    drop(guard);
}
fn main() {}
