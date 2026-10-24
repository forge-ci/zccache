#![allow(dead_code)]
use zccache_core::cache_root_lock::CacheRootWriterLease;
fn direct(lease: CacheRootWriterLease) {
    use std::mem::forget as discard;
    discard(lease);
}
fn forget_value<T>(value: T) {
    std::mem::forget(value);
}
fn generic(lease: CacheRootWriterLease) {
    forget_value(lease);
}
fn erased(lease: CacheRootWriterLease) {
    let _erased: Box<dyn std::any::Any> = Box::new(lease);
}
fn suppressed(lease: CacheRootWriterLease) {
    let _hidden = std::mem::ManuallyDrop::new(lease);
}
unsafe fn overwritten(slot: *mut CacheRootWriterLease, replacement: CacheRootWriterLease) {
    std::ptr::write(slot, replacement);
}
fn erase_value<T: 'static>(value: T) -> Box<dyn std::any::Any> {
    Box::new(value)
}
fn generic_erased(lease: CacheRootWriterLease) {
    let _erased = erase_value(lease);
}
fn main() {}
fn shared(lease: std::sync::Arc<CacheRootWriterLease>) {
    std::mem::forget(lease);
}
fn shared_generic(lease: std::sync::Arc<CacheRootWriterLease>) {
    forget_value(lease);
}
fn shared_erased(lease: std::sync::Arc<CacheRootWriterLease>) {
    let _erased: Box<dyn std::any::Any> = Box::new(lease);
}
