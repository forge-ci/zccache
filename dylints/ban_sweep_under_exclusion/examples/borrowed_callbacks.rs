#![allow(dead_code)]
fn invoke<F: Fn()>(callback: F) {
    callback();
}
fn bulk() {
    let _ = std::fs::read("output");
}
fn readers(guard: zccache_core::sync::PublicationReadPermit) {
    let callback = || {
        let _ = std::fs::write("output", b"bytes");
    };
    invoke(&callback);
    invoke(&bulk);
    drop(guard);
}
fn main() {}
