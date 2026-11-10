#![allow(dead_code)]
use kernal_api::platform::process::{foreground_output as capture, DaemonChild, SpawnedChild};
fn bypass(command: &mut std::process::Command, daemon: &mut DaemonChild, child: &mut SpawnedChild) {
    let _ = capture(command);
    let wait = DaemonChild::wait_within;
    let _ = wait(daemon, std::time::Duration::from_secs(10));
    let _ = child.wait_within(std::time::Duration::from_secs(10));
    let _spawn = kernal_api::platform::process::spawn_sync;
}
fn main() {}
