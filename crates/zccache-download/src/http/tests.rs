use std::future::Future;

use kernal_api::async_engine::CancellationSource;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

const SHORT: HttpPolicy = HttpPolicy {
    headers: Duration::from_millis(300),
    stall: Duration::from_millis(300),
    total: None,
};
/// Far past every budget here: reaching it means the budget did not fire.
const HANG: Duration = Duration::from_secs(30);

/// Serves `respond` to each request, then holds the socket open silently.
async fn stalled_server<F, Fut>(respond: F) -> String
where
    F: Fn(tokio::net::TcpStream) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).await;
            tokio::spawn(respond(stream));
        }
    });
    format!("http://{addr}/asset?sig=SECRET#frag")
}

async fn silent(mut stream: tokio::net::TcpStream) {
    let _ = stream.read_to_end(&mut Vec::new()).await;
}

async fn partial_body(mut stream: tokio::net::TcpStream) {
    let _ = stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc")
        .await;
    let _ = stream.read_to_end(&mut Vec::new()).await;
}

fn client() -> HttpClient {
    HttpClient::new("test", false, Duration::from_secs(5)).unwrap()
}

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(HANG, future)
        .await
        .expect("the budget or cancellation did not fire")
}

#[tokio::test]
async fn silent_server_expires_at_the_header_budget_without_leaking_the_query() {
    let url = stalled_server(silent).await;
    let Err(error) = within(client().get(&url, SHORT).send(&uncancelled())).await else {
        panic!("a silent server must not produce a response");
    };
    assert!(
        matches!(
            error,
            HttpError::Deadline {
                phase: HttpPhase::Headers,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(!error.to_string().contains("SECRET"), "{error}");
}

#[tokio::test]
async fn stalled_body_expires_at_the_stall_budget() {
    let url = stalled_server(partial_body).await;
    let mut response = within(client().get(&url, SHORT).send(&uncancelled()))
        .await
        .unwrap();
    assert!(within(response.next_chunk()).await.unwrap().is_some());
    let Err(error) = within(response.next_chunk()).await else {
        panic!("a stalled body must not produce a chunk");
    };
    assert!(
        matches!(
            error,
            HttpError::Deadline {
                phase: HttpPhase::BodyStall,
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn slow_progressing_body_outlives_the_stall_budget() {
    // Total transfer time (4 s) exceeds the stall budget (3 s), yet each gap
    // (0.5 s) is well inside it. Client and server share this runtime, so host
    // load delays both sides together.
    const CHUNKS: u64 = 8;
    let url = stalled_server(|mut stream| async move {
        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {CHUNKS}\r\n\r\n");
        let _ = stream.write_all(head.as_bytes()).await;
        for _ in 0..CHUNKS {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = stream.write_all(b"x").await;
            let _ = stream.flush().await;
        }
    })
    .await;
    let policy = HttpPolicy {
        stall: Duration::from_secs(3),
        headers: Duration::from_secs(3),
        total: None,
    };
    let started = Instant::now();
    let response = client().get(&url, policy).send(&uncancelled()).await;
    let body = within(response.unwrap().read_to_end()).await.unwrap();
    assert_eq!(body.len() as u64, CHUNKS);
    assert!(started.elapsed() > policy.stall, "{:?}", started.elapsed());
}

#[tokio::test]
async fn total_budget_caps_a_control_exchange_that_keeps_progressing() {
    let url = stalled_server(|mut stream| async move {
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await;
        loop {
            if stream.write_all(b"1\r\nx\r\n").await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let policy = HttpPolicy {
        total: Some(Duration::from_millis(400)),
        headers: Duration::from_secs(5),
        stall: Duration::from_secs(5),
    };
    let response = within(client().get(&url, policy).send(&uncancelled())).await;
    let error = within(response.unwrap().read_to_end()).await.unwrap_err();
    assert!(
        matches!(
            error,
            HttpError::Deadline {
                phase: HttpPhase::Total,
                ..
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn cancellation_interrupts_the_header_wait() {
    let url = stalled_server(silent).await;
    let source = CancellationSource::new();
    let token = source.token();
    let patient = HttpPolicy {
        headers: HANG * 2,
        ..SHORT
    };
    let client = client();
    let send = client.get(&url, patient).send(&token);
    tokio::pin!(send);
    tokio::select! {
        _ = &mut send => panic!("a silent server must not answer"),
        () = tokio::time::sleep(Duration::from_millis(100)) => source.cancel(),
    }
    assert!(matches!(within(send).await, Err(HttpError::Cancelled)));
}

#[tokio::test]
async fn cancellation_interrupts_a_stalled_body() {
    let url = stalled_server(partial_body).await;
    let source = CancellationSource::new();
    let patient = HttpPolicy {
        stall: HANG * 2,
        ..SHORT
    };
    let mut response = within(client().get(&url, patient).send(&source.token()))
        .await
        .unwrap();
    assert!(within(response.next_chunk()).await.unwrap().is_some());
    source.cancel();
    assert!(matches!(
        within(response.next_chunk()).await,
        Err(HttpError::Cancelled)
    ));
}

#[tokio::test]
async fn control_bodies_are_size_limited() {
    let url = stalled_server(|mut stream| async move {
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n12345678")
            .await;
    })
    .await;
    let response = within(client().get(&url, SHORT).send(&uncancelled())).await;
    let error = within(response.unwrap().read_limited(4)).await.unwrap_err();
    assert!(matches!(error, HttpError::BodyTooLarge { .. }), "{error:?}");
    assert!(!error.to_string().contains("SECRET"), "{error}");
}

#[tokio::test]
async fn transport_errors_omit_the_url() {
    // Bind then drop: the port refuses connections.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let url = format!("http://user:PASSWORD@{addr}/x?sig=SECRET");
    let Err(error) = within(client().get(&url, SHORT).send(&uncancelled())).await else {
        panic!("a closed port must not answer");
    };
    let message = error.to_string();
    assert!(
        !message.contains("SECRET") && !message.contains("PASSWORD"),
        "{message}"
    );
}

#[test]
fn redaction_keeps_only_the_location() {
    assert_eq!(
        redact_url("https://u:p@example.com:8443/a/b?token=t#f"),
        "https://example.com:8443/a/b"
    );
    assert_eq!(redact_url("not a url"), "<unparsable url>");
}

#[test]
fn an_upload_extends_only_the_header_budget() {
    let client = client();
    let request = client
        .patch("http://127.0.0.1/x", TRANSFER)
        .body(vec![0; 10 * 1024 * 1024]);
    assert_eq!(
        request.policy.headers,
        TRANSFER.headers + Duration::from_secs(160)
    );
    assert_eq!(request.policy.stall, TRANSFER.stall);
}
