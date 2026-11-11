// compile-flags: --edition=2021
#![allow(dead_code)]
use std::sync::RwLockWriteGuard as Guard;
fn guarded(guard: Guard<'_, ()>) {
    let _ = std::fs::read_dir("owned-cache");
    drop(guard);
}
fn main() {}
