// compile-flags: --crate-name=zccache_daemon_core
#![allow(dead_code)]
fn invoke<F: Fn()>(callback: F) {
    callback();
}
fn remove() {
    let _ = std::fs::remove_file("live-cache");
}
fn readers(guard: zccache_core::sync::PublicationReadPermit) {
    let callback = || {
        let _ = std::fs::remove_file("live-cache");
    };
    invoke(&callback);
    invoke(&remove);
    drop(guard);
}
fn erased(guard: zccache_core::sync::PublicationReadPermit, callback: fn()) {
    invoke(&callback);
    drop(guard);
}
fn main() {}

mod unrelated {
    pub mod spawn_exclusion {
        pub struct MaterializationPermit(pub zccache_core::sync::PublicationCommitPermit);
    }
}
fn spoofed(permit: unrelated::spawn_exclusion::MaterializationPermit) {
    let _ = std::fs::read("still-critical");
    drop(permit);
}
