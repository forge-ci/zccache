use std::time::Duration;
use zccache_core::sync::PublicationBarrier;
pub async fn drain_durable_state_for_shutdown(barrier: &PublicationBarrier) {
    poll();
    let permit = barrier.drain_for(Duration::from_secs(1)).await;
    drop(permit);
}

// An unrelated free function must not be queried as a trait method.
fn poll() {}
