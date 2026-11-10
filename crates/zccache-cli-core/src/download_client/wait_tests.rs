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
        conn,
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
