#![allow(dead_code)]
use std::time::Duration;

fn bridge(runtime: &kernal_api::async_engine::Runtime) {
    let _ = runtime.run_within(async {}, Duration::from_secs(1));
}

async fn admission(gate: &kernal_api::async_engine::Semaphore) {
    let _ = gate.acquire_within(Duration::from_secs(1)).await;
}

fn main() {}
