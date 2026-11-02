#![allow(dead_code)]
use std::process::Child as SyncChild;
use tokio::process::{Child as AsyncChild, Command as AsyncCommand};
async fn asynchronous(command: &mut AsyncCommand, child: &mut AsyncChild) {
    let spawn = AsyncCommand::spawn;
    let _ = spawn(command);
    let _ = AsyncChild::wait(child).await;
    let _ = command.output().await;
}
fn synchronous(child: &mut SyncChild) {
    let wait = SyncChild::wait;
    let _ = wait(child);
}
fn main() {}
