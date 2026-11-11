#![allow(dead_code, forgetting_copy_types)]
use std::{marker::PhantomData, sync::Weak};
use zccache_core::sync::PublicationCommitPermit as Commit;
fn weak(value: Weak<Commit>) {
    let _ = std::fs::read_dir("owned-cache");
    drop(value);
}
fn phantom(value: PhantomData<Commit>) {
    let _ = std::fs::read_dir("owned-cache");
    let _ = value;
}
fn non_owning_forget(value: Weak<Commit>) {
    std::mem::forget(value);
    std::mem::forget(PhantomData::<Commit>);
}
fn main() {}
