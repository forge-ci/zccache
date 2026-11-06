#![allow(dead_code)]
use std::{sync::Arc, time::Duration};
use zccache_core::{
    blocking_sync::Mutex,
    sync::{Gate, State},
};
fn native_guard() {
    let lock = Mutex::new(0);
    *lock.lock_for(Duration::from_secs(1)).unwrap() += 1;
}
async fn callback_state(state: &State<u32>) {
    let owned = state.with_for(Duration::from_secs(1), |value| *value).await;
    std::future::ready(()).await;
    println!("{owned}");
}
async fn operation_permit(gate: &Arc<Gate>) {
    let permit = gate.acquire_for(Duration::from_secs(1)).await;
    std::future::ready(()).await;
    drop(permit);
}
fn main() {}
