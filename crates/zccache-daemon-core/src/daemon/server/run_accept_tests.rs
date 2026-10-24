//! Accept-loop watchdog regressions.

use std::{sync::Arc, time::Duration};

use super::{wait_for_accept_or_shutdown, AcceptWait};
use tokio::sync::{oneshot, Notify};

#[tokio::test]
async fn watchdog_ticks_do_not_cancel_the_pending_accept() {
    let (accepted_tx, accepted_rx) = oneshot::channel();
    let waiter = tokio::spawn(wait_for_accept_or_shutdown(
        async { accepted_rx.await.expect("accept sender remains live") },
        std::future::pending(),
        Duration::from_millis(5),
    ));

    tokio::time::sleep(Duration::from_millis(20)).await;
    accepted_tx
        .send(7_u8)
        .expect("watchdog must not drop the accept future");

    assert!(matches!(waiter.await.unwrap(), AcceptWait::Accepted(7)));
}

#[tokio::test]
async fn watchdog_ticks_do_not_lose_the_shutdown_waiter() {
    let shutdown = Arc::new(Notify::new());
    let shutdown_wait = Arc::clone(&shutdown);
    let waiter = tokio::spawn(wait_for_accept_or_shutdown(
        std::future::pending::<()>(),
        async move { shutdown_wait.notified().await },
        Duration::from_millis(5),
    ));

    tokio::time::sleep(Duration::from_millis(20)).await;
    shutdown.notify_waiters();

    assert!(matches!(waiter.await.unwrap(), AcceptWait::Shutdown));
}
