use super::*;
use std::time::Duration;

/// #1967: the budget bounds the whole request/response, even when a daemon
/// receives a wait request and then stops servicing its connection.
#[tokio::test(start_paused = true)]
async fn stalled_download_wait_expires_within_its_transport_budget() {
    let endpoint = crate::ipc::unique_test_endpoint();
    let mut listener = crate::ipc::IpcListener::bind(&endpoint).unwrap();
    let server = tokio::spawn(async move {
        let mut conn = crate::download_ipc::DownloadIpcConnection::from_connection(
            listener.accept().await.unwrap(),
        );
        assert_eq!(
            conn.recv::<Request>().await.unwrap(),
            Some(Request::DownloadWait { timeout_ms: 25 })
        );
        std::future::pending::<()>().await;
    });
    let conn = ClientConn::connect(&endpoint).await.unwrap();
    let mut handle = AsyncDownloadHandle {
        conn: Some(conn),
        initiator: false,
        download_id: "test".into(),
    };
    let timeout = Duration::from_millis(25);
    let started = tokio::time::Instant::now();
    let error = handle.wait(timeout).await.unwrap_err();
    assert!(error.contains("wait budget"), "{error}");
    assert_eq!(started.elapsed(), timeout + crate::core::wait::PROBE);
    drop(handle);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn late_wait_reply_cannot_satisfy_a_later_command() {
    let endpoint = crate::ipc::unique_test_endpoint();
    let mut listener = crate::ipc::IpcListener::bind(&endpoint).unwrap();
    let (received_tx, received_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let (late_tx, late_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut conn = crate::download_ipc::DownloadIpcConnection::from_connection(
            listener.accept().await.unwrap(),
        );
        assert!(matches!(
            conn.recv::<Request>().await.unwrap(),
            Some(Request::DownloadWait { .. })
        ));
        received_tx.send(()).unwrap();
        release_rx.await.unwrap();
        let status = DownloadStatus {
            phase: crate::download::DownloadPhase::Downloading,
            total_bytes: None,
            downloaded_bytes: 0,
            percentage: None,
            active_clients: 1,
            destination: crate::core::NormalizedPath::new("late-reply"),
            source_url: "test".into(),
            error: None,
        };
        let _ = conn.send(&Response::DownloadStatusResult { status }).await;
        late_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    let conn = ClientConn::connect(&endpoint).await.unwrap();
    let handle = AsyncDownloadHandle {
        conn: Some(conn),
        initiator: false,
        download_id: "test".into(),
    };
    let timeout = Duration::from_millis(25);
    let waiter = tokio::spawn(async move {
        let mut handle = handle;
        let result = handle.wait(timeout).await;
        (handle, result)
    });
    received_rx.await.unwrap();
    tokio::time::pause();
    tokio::time::advance(timeout + crate::core::wait::PROBE).await;
    let (mut handle, result) = waiter.await.unwrap();
    assert!(result.is_err());
    release_tx.send(()).unwrap();
    late_rx.await.unwrap();
    assert!(
        handle.status().await.is_err(),
        "a late wait response must not satisfy status"
    );
    assert!(
        handle.cancel().await.is_err(),
        "a failed handle must stay closed"
    );
    drop(handle);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[test]
fn synchronous_failed_exchange_cannot_reuse_a_late_reply() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (conn, server) = crate::core::wait::block_on_for(
        &runtime,
        async {
            let endpoint = crate::ipc::unique_test_endpoint();
            let mut listener = crate::ipc::IpcListener::bind(&endpoint).unwrap();
            let server = tokio::spawn(async move {
                let mut conn = crate::download_ipc::DownloadIpcConnection::from_connection(
                    listener.accept().await.unwrap(),
                );
                assert_eq!(
                    conn.recv::<Request>().await.unwrap(),
                    Some(Request::DownloadStatus)
                );
                conn.send(&Response::Pong).await.unwrap();
                let status = DownloadStatus {
                    phase: crate::download::DownloadPhase::Downloading,
                    total_bytes: None,
                    downloaded_bytes: 0,
                    percentage: None,
                    active_clients: 1,
                    destination: crate::core::NormalizedPath::new("late-reply"),
                    source_url: "test".into(),
                    error: None,
                };
                let _ = conn.send(&Response::DownloadStatusResult { status }).await;
                std::future::pending::<()>().await;
            });
            (ClientConn::connect(&endpoint).await.unwrap(), server)
        },
        crate::core::wait::PROBE,
    )
    .unwrap();
    let mut handle = DownloadHandle {
        runtime,
        conn: Some(conn),
        initiator: false,
        download_id: "test".into(),
    };
    assert!(
        handle.status().is_err(),
        "the first response violates the protocol"
    );
    assert!(
        handle.cancel().is_err(),
        "the stale status must not satisfy cancel"
    );
    server.abort();
    let result =
        crate::core::wait::block_on_for(&handle.runtime, server, crate::core::wait::PROBE).unwrap();
    assert!(result.unwrap_err().is_cancelled());
}
