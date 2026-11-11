#![allow(dead_code)]
use zccache_core::sync::PublicationReadPermit;
fn invoke<F: FnOnce()>(operation: F) {
    operation();
}
fn read(permit: PublicationReadPermit) {
    invoke(|| {
        let _ = std::fs::read("owned-cache");
    });
    drop(permit);
}
fn iterator_read(permit: PublicationReadPermit) {
    std::iter::once("cache").for_each(|path| {
        let _ = std::fs::read(path);
    });
    drop(permit);
}
fn main() {}
