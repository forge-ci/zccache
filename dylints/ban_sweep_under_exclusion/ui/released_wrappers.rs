// compile-flags: --edition=2021
#![allow(dead_code)]
use std::sync::RwLockWriteGuard as Guard;
struct Pair<'a> { first: Guard<'a, ()>, second: Guard<'a, ()> }
fn all_released(pair: Pair<'_>) {
    let first = pair.first;
    let second = pair.second;
    drop(first);
    drop(second);
    let _ = std::fs::read_dir("owned-cache");
}
fn extracted(result: Result<Guard<'_, ()>, ()>) {
    if let Ok(guard) = result { drop(guard); }
    let _ = std::fs::read_dir("owned-cache");
}
fn main() {}
