#![allow(dead_code)]
use zccache_core::sync::PublicationReadPermit;

unsafe fn overwrite(slot: *mut PublicationReadPermit, replacement: PublicationReadPermit) {
    use std::ptr::write as overwrite_value;
    overwrite_value(slot, replacement);
}
unsafe fn duplicate(source: *const PublicationReadPermit) {
    let read = std::ptr::read::<PublicationReadPermit>;
    drop(read(source));
}
unsafe fn copy(source: *const PublicationReadPermit, destination: *mut PublicationReadPermit) {
    std::ptr::copy_nonoverlapping(source, destination, 1);
}
unsafe fn truncate(guards: &mut Vec<PublicationReadPermit>) {
    Vec::set_len(guards, 0);
}
unsafe fn ordinary_memory(source: *const u8, destination: *mut u8, bytes: &mut Vec<u8>) {
    std::ptr::write(destination, std::ptr::read(source));
    std::ptr::copy_nonoverlapping(source, destination, 1);
    bytes.set_len(0);
}
fn main() {}
