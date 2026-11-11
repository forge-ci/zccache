#![allow(dead_code)]
use zccache_core::sync::PublicationReadPermit;
fn release_then_drop<T>(permit: PublicationReadPermit, value: T) {
    drop(permit);
    drop(value);
}
fn transferred<T>(permit: PublicationReadPermit, value: T) {
    release_then_drop(permit, value);
}
fn main() {}
