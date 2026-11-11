// compile-flags: --edition=2021
#![allow(dead_code)]
use std::sync::RwLockWriteGuard as Guard;
fn invoke<F: FnOnce()>(callback: F) {
    callback();
}
fn held(guard: Guard<'_, ()>) {
    invoke(|| {});
    invoke(|| { let _ = std::fs::read_dir("owned-cache"); });
    invoke(|| { let _ = std::fs::read_dir("owned-cache"); });
    drop(guard);
}
fn main() {}
