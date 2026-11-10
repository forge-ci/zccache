#![allow(dead_code)]
use zccache_core::sync::PublicationReadPermit;
fn release_then_drop<T>(permit: PublicationReadPermit, value: T) {
    drop(permit);
    drop(value);
}
fn retained<T>(first: PublicationReadPermit, second: PublicationReadPermit, value: T) {
    release_then_drop(first, value);
    drop(second);
}
fn wrong_order<T>(permit: PublicationReadPermit, value: T) {
    drop(value);
    drop(permit);
}
fn calls_wrong<T>(permit: PublicationReadPermit, value: T) {
    wrong_order(permit, value);
}
fn main() {}
