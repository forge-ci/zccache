#![allow(dead_code)]
use kernal_api::{ProcessSession as Session, ProcessSessionOptions, SpawnSpec as Tool};
async fn bypass(tool: Tool, session: &Session, options: ProcessSessionOptions) {
    let _ = tool.spawn_session(options).await;
    let _ = Session::wait(session).await;
    let wait = Session::next_output;
    let _ = wait(session).await;
    let spawn = Tool::spawn;
    let _ = spawn(Tool::new("compiler")).await;
}
fn main() {}
