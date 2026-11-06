#![allow(dead_code)]
use kernal_api::platform::process::{foreground_output as capture, DaemonChild, SpawnedChild};
fn bypass(command: &mut std::process::Command, daemon: &mut DaemonChild, child: &mut SpawnedChild) {
    let _ = capture(command);
    let wait = DaemonChild::wait;
    let _ = wait(daemon);
    let _ = child.wait();
    let _spawn = kernal_api::platform::process::spawn_sync;
}
fn main() {}
