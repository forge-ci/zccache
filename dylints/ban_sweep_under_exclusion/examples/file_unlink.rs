#![allow(dead_code)]
use std::fs::remove_file as unlink;
use zccache_core::sync::PublicationReadPermit;

fn held_alias(permit: PublicationReadPermit) {
    let _ = unlink("retired");
    drop(permit);
}
fn held_item(permit: PublicationReadPermit) {
    let delete = std::fs::remove_file::<&str>;
    let _ = delete("retired");
    drop(permit);
}
fn released(permit: PublicationReadPermit) {
    drop(permit);
    let delete = std::fs::remove_file::<&str>;
    let _ = delete("retired");
}
fn main() {}
