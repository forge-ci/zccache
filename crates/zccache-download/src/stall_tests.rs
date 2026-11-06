//! #1968: every engine wait stops on a local socket that goes silent.

use super::*;

use kernal_api::async_engine::CancellationSource;
use tokio::net::TcpListener;

/// How the server answers a method.
#[derive(Clone, Copy)]
enum Mode {
    Silent,
    HeadRefusedThenSilent,
    PartialBody,
    /// HEAD advertises ranges; each range request then gets no answer.
    RangedThenSilent,
}

async fn server(mode: Mode) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    let head = buf.starts_with(b"HEAD");
                    let reply: &[u8] = match (mode, head) {
                        (Mode::Silent, _)
                        | (Mode::HeadRefusedThenSilent, false)
                        | (Mode::RangedThenSilent, false) => b"",
                        (Mode::HeadRefusedThenSilent, true) => {
                            b"HTTP/1.1 405 No\r\nContent-Length: 0\r\n\r\n"
                        }
                        (Mode::PartialBody, true) => {
                            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n"
                        }
                        (Mode::PartialBody, false) => {
                            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc"
                        }
                        (Mode::RangedThenSilent, true) => {
                            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nAccept-Ranges: bytes\r\n\r\n"
                        }
                    };
                    let _ = stream.write_all(reply).await;
                }
            });
        }
    });
    format!("http://{addr}/payload?sig=SECRET")
}

const SHORT: HttpPolicy = HttpPolicy {
    headers: Duration::from_millis(300),
    stall: Duration::from_millis(300),
    total: None,
};

async fn run(mode: Mode, cancel_after: Option<Duration>) -> Result<Option<u64>, DownloadError> {
    let url = server(mode).await;
    let dir = tempfile::tempdir().unwrap();
    let source = CancellationSource::new();
    let token = source.token();
    let options = DownloadOptions {
        allow_insecure_http: true,
        max_connections: Some(2),
        min_segment_size: Some(10),
        ..DownloadOptions::default()
    };
    // Cancellation runs need budgets that cannot be what stops the download.
    let policy = if cancel_after.is_some() {
        http::TRANSFER
    } else {
        SHORT
    };
    let budgets = Budgets {
        connect: Duration::from_secs(5),
        probe: policy,
        body: policy,
    };
    let (destination, metadata) = (dir.path().join("out"), dir.path().join("meta"));
    let download = download_with_budgets(
        &url,
        &destination,
        &metadata,
        &options,
        Arc::new(|_, _, _| {}),
        token,
        budgets,
    );
    if let Some(delay) = cancel_after {
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            source.cancel();
        });
    }
    tokio::time::timeout(Duration::from_secs(30), download)
        .await
        .expect("the budget or cancellation did not fire")
}

fn assert_expired(result: Result<Option<u64>, DownloadError>, phase: &str) {
    match result {
        Err(DownloadError::Http(message)) => {
            assert!(message.contains(phase), "{message}");
            assert!(!message.contains("SECRET"), "{message}");
        }
        other => panic!("expected a {phase} expiry, got {other:?}"),
    }
}

#[tokio::test]
async fn head_probe_expires_on_a_silent_server() {
    assert_expired(run(Mode::Silent, None).await, "response-header");
}

#[tokio::test]
async fn fallback_range_get_expires_on_a_silent_server() {
    assert_expired(
        run(Mode::HeadRefusedThenSilent, None).await,
        "response-header",
    );
}

#[tokio::test]
async fn single_stream_body_stall_expires() {
    assert_expired(run(Mode::PartialBody, None).await, "body-stall");
}

#[tokio::test]
async fn segment_header_wait_expires() {
    assert_expired(run(Mode::RangedThenSilent, None).await, "response-header");
}

#[tokio::test]
async fn cancelling_during_the_header_wait_reports_cancelled() {
    let result = run(Mode::Silent, Some(Duration::from_millis(200))).await;
    assert!(
        matches!(result, Err(DownloadError::Cancelled)),
        "{result:?}"
    );
}

#[tokio::test]
async fn cancelling_during_a_stalled_stream_reports_cancelled() {
    let result = run(Mode::PartialBody, Some(Duration::from_millis(300))).await;
    assert!(
        matches!(result, Err(DownloadError::Cancelled)),
        "{result:?}"
    );
}
