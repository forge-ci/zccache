#![allow(dead_code, dropping_copy_types, undropped_manually_drops)]
use std::{any::Any, mem::ManuallyDrop};
use zccache_core::sync::PublicationCommitPermit as Commit;
fn forgotten(permit: Commit) {
    std::mem::forget(permit);
    let _ = std::fs::read_dir("cache");
}
fn captured_forget() {
    let _ = std::mem::forget::<Commit>;
}
fn manual(permit: Commit) {
    drop(ManuallyDrop::new(permit));
    let _ = std::fs::read_dir("cache");
}
fn raw(permit: Commit) {
    let _ = Box::into_raw(Box::new(permit));
    let _ = std::fs::read_dir("cache");
}
fn erased(permit: Commit) {
    let erased: Box<dyn Any> = Box::new(permit);
    let _ = std::fs::read_dir("cache");
    drop(erased);
}
fn uninitialized(permit: Commit) {
    drop(std::mem::MaybeUninit::new(permit));
    let _ = std::fs::read_dir("cache");
}
fn leaked_box(permit: Commit) {
    let _ = Box::leak(Box::new(permit));
    let _ = std::fs::read_dir("cache");
}
fn transmuted(permit: Commit) {
    let manual = unsafe { std::mem::transmute::<Commit, ManuallyDrop<Commit>>(permit) };
    drop(manual);
    let _ = std::fs::read_dir("cache");
}
fn main() {}
