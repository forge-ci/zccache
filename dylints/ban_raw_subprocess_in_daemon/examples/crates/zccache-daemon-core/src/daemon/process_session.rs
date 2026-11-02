#![allow(dead_code)]
use kernal_api::ProcessSession;
use std::time::Duration;
pub async fn bounded(session: &ProcessSession, budget: Duration) {
    let _ = kernal_api::async_engine::timeout(budget, session.wait()).await;
}
pub async fn unbounded(session: &ProcessSession) {
    let _ = session.wait().await;
}
pub async fn optional(session: &ProcessSession, _budget: Option<Duration>) {
    let wait = ProcessSession::wait;
    let _ = wait(session).await;
}

fn main() {}
