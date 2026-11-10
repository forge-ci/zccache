#![allow(dead_code, deprecated)]
use kernal_api::async_engine::{Runtime as Engine, Semaphore as Admission};
use kernal_api::run_bounded_command_async as capture;

fn bridges(runtime: &Engine) {
    runtime.run(async {});
    let _alias = Engine::run::<std::future::Ready<()>>;
    let _capture = kernal_api::PlatformChild::wait_with_output_bounded;
    let _receive = kernal_api::async_engine::Receiver::<()>::recv;
    let _send = kernal_api::async_engine::Sender::<()>::send;
    let _ = capture(kernal_api::SpawnSpec::new("probe"), None, 1024);
}

async fn admission(gate: &Admission) {
    let _permit = gate.acquire().await;
    let _function_item = Admission::acquire_many;
}

fn main() {}
