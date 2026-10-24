// compile-flags: --edition=2021
#![allow(dead_code)]
use std::sync::RwLockWriteGuard as Guard;
fn released(guard: Guard<'_, ()>) {
    drop(guard);
    let _ = std::fs::read_dir("owned-cache");
}
fn main() {}
