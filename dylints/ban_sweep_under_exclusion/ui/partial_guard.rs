// compile-flags: --edition=2021
#![allow(dead_code)]
use std::sync::RwLockWriteGuard as Guard;
struct Pair<'a> { first: Guard<'a, ()>, second: Guard<'a, ()> }
fn partially_released(pair: Pair<'_>) {
    let first = pair.first;
    drop(first);
    let _ = std::fs::read_dir("owned-cache");
    // The other field drops implicitly when this function returns.
}
fn main() {}
