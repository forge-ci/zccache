// compile-flags: --crate-name=zccache_daemon_core
#![allow(dead_code)]
#[path = "../../../crates/zccache-daemon-core/src/daemon/server/persist/staged_store/immediate_children.rs"]
mod immediate_children;
use immediate_children::ImmediateChildren as Cursor;
use std::{path::Path, time::Duration};
fn wrong_owner() {
    let _ = Cursor::open_for(Path::new("cache"), Duration::from_secs(1));
}
fn capture() {
    let _ = Cursor::open_for;
}
fn main() {}
