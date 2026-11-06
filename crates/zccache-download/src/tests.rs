use super::*;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc;

use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Clone)]
struct TestServerConfig {
    body: Arc<Vec<u8>>,
    accept_ranges: bool,
    send_content_length: bool,
    chunk_size: usize,
    chunk_delay_ms: u64,
    etag: Option<String>,
}

struct TestServer {
    url: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    async fn start(config: TestServerConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break; };
                        let config = config.clone();
                        tokio::spawn(async move {
                            let _ = handle_test_connection(stream, config).await;
                        });
                    }
                }
            }
        });
        Self {
            url: format!("http://{addr}/payload.bin"),
            shutdown: Some(shutdown_tx),
            task,
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.task.abort();
    }
}

async fn handle_test_connection(
    mut stream: tokio::net::TcpStream,
    config: TestServerConfig,
) -> Result<(), std::io::Error> {
    let mut request = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buf[..n]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    let request_text = String::from_utf8_lossy(&request);
    let mut lines = request_text.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();

    let range_header = request_text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("range") {
            Some(value.trim().to_string())
        } else {
            None
        }
    });

    let mut status_line = "HTTP/1.1 200 OK\r\n".to_string();
    let mut body = (*config.body).clone();

    if let Some(range) = range_header {
        if config.accept_ranges {
            if let Some((start, end)) = parse_range(&range, body.len() as u64) {
                status_line = "HTTP/1.1 206 Partial Content\r\n".to_string();
                body = body[start as usize..=end as usize].to_vec();
            }
        }
    }

    let mut headers = String::new();
    headers.push_str("Connection: close\r\n");
    headers.push_str("Content-Type: application/octet-stream\r\n");
    if config.accept_ranges {
        headers.push_str("Accept-Ranges: bytes\r\n");
    }
    if config.send_content_length {
        headers.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    if let Some(etag) = &config.etag {
        headers.push_str(&format!("ETag: {etag}\r\n"));
    }

    stream.write_all(status_line.as_bytes()).await?;
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(b"\r\n").await?;

    if method.eq_ignore_ascii_case("HEAD") {
        stream.flush().await?;
        return Ok(());
    }

    if config.chunk_size == 0 {
        stream.write_all(&body).await?;
    } else {
        for chunk in body.chunks(config.chunk_size) {
            stream.write_all(chunk).await?;
            stream.flush().await?;
            if config.chunk_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(config.chunk_delay_ms)).await;
            }
        }
    }
    stream.flush().await?;
    Ok(())
}

fn parse_range(header: &str, total_len: u64) -> Option<(u64, u64)> {
    let range = header.strip_prefix("bytes=")?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse::<u64>().ok()?;
    let end = if end.is_empty() {
        total_len.checked_sub(1)?
    } else {
        end.parse::<u64>().ok()?
    };
    if start > end || end >= total_len {
        return None;
    }
    Some((start, end))
}

fn progress_recorder() -> (
    ProgressCallback,
    mpsc::Receiver<(u64, Option<u64>, DownloadPhase)>,
) {
    let (tx, rx) = mpsc::channel();
    let callback = Arc::new(move |downloaded, total, phase| {
        let _ = tx.send((downloaded, total, phase));
    });
    (callback, rx)
}

fn expected_temp_path(destination: &Path) -> PathBuf {
    destination.with_extension(format!(
        "{}part",
        destination
            .extension()
            .map(|ext| format!("{}.", ext.to_string_lossy()))
            .unwrap_or_default()
    ))
}

#[tokio::test]
async fn download_single_stream_from_dummy_server() {
    let server = TestServer::start(TestServerConfig {
        body: Arc::new(b"hello download world".to_vec()),
        accept_ranges: false,
        send_content_length: true,
        chunk_size: 0,
        chunk_delay_ms: 0,
        etag: Some("\"single\"".to_string()),
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("artifact.bin");
    let metadata_dir = dir.path().join("metadata");
    let (progress, rx) = progress_recorder();

    let result = download_to_path(
        &server.url,
        &destination,
        &metadata_dir,
        &DownloadOptions {
            force: false,
            max_connections: Some(1),
            min_segment_size: Some(1024),
            // Local test server has no certificate (#1172).
            allow_insecure_http: true,
        },
        progress,
        CancellationSource::new().token(),
    )
    .await
    .unwrap();

    assert_eq!(result, Some(20));
    assert_eq!(
        tokio::fs::read(&destination).await.unwrap(),
        b"hello download world"
    );

    let events: Vec<_> = rx.try_iter().collect();
    assert!(events
        .iter()
        .any(|(_, _, phase)| *phase == DownloadPhase::Completed));
}

#[tokio::test]
async fn download_segmented_from_dummy_server_on_random_port() {
    let body: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
    let server = TestServer::start(TestServerConfig {
        body: Arc::new(body.clone()),
        accept_ranges: true,
        send_content_length: true,
        chunk_size: 4096,
        chunk_delay_ms: 0,
        etag: Some("\"segmented\"".to_string()),
    })
    .await;

    let parsed: SocketAddr = server
        .url
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        parsed.port() > 0,
        "server must resolve a random port from bind(0)"
    );

    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("segmented.bin");
    let metadata_dir = dir.path().join("segments");

    let result = download_to_path(
        &server.url,
        &destination,
        &metadata_dir,
        &DownloadOptions {
            force: false,
            max_connections: Some(4),
            min_segment_size: Some(1024),
            // Local test server has no certificate (#1172).
            allow_insecure_http: true,
        },
        Arc::new(|_, _, _| {}),
        CancellationSource::new().token(),
    )
    .await
    .unwrap();

    assert_eq!(result, Some(body.len() as u64));
    assert_eq!(tokio::fs::read(&destination).await.unwrap(), body);
    assert!(
        tokio::fs::metadata(&metadata_dir).await.is_err(),
        "segment metadata dir should be removed after success"
    );
}

#[tokio::test]
async fn download_without_content_length_still_completes() {
    let body = b"unknown length body".to_vec();
    let server = TestServer::start(TestServerConfig {
        body: Arc::new(body.clone()),
        accept_ranges: false,
        send_content_length: false,
        chunk_size: 5,
        chunk_delay_ms: 0,
        etag: None,
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("unknown.bin");
    let metadata_dir = dir.path().join("meta");
    let (progress, rx) = progress_recorder();

    let result = download_to_path(
        &server.url,
        &destination,
        &metadata_dir,
        &DownloadOptions {
            // Local test server has no certificate (#1172).
            allow_insecure_http: true,
            ..DownloadOptions::default()
        },
        progress,
        CancellationSource::new().token(),
    )
    .await
    .unwrap();

    assert_eq!(result, Some(body.len() as u64));
    assert_eq!(tokio::fs::read(&destination).await.unwrap(), body);
    let events: Vec<_> = rx.try_iter().collect();
    assert!(events.iter().any(|(_, total, _)| total.is_none()));
}

#[tokio::test]
async fn cancelling_download_removes_temp_file_and_metadata() {
    let body: Vec<u8> = (0..128 * 1024).map(|i| (i % 251) as u8).collect();
    let server = TestServer::start(TestServerConfig {
        body: Arc::new(body),
        accept_ranges: false,
        send_content_length: true,
        chunk_size: 1024,
        chunk_delay_ms: 10,
        etag: Some("\"cancel\"".to_string()),
    })
    .await;

    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("cancel.bin");
    let metadata_dir = dir.path().join("cancel-meta");
    let temp_path = expected_temp_path(&destination);
    let cancel = CancellationSource::new();
    let cancel_clone = cancel.token();
    let (progress, rx) = progress_recorder();

    let task = tokio::spawn(async move {
        download_to_path(
            &server.url,
            &destination,
            &metadata_dir,
            &DownloadOptions {
                force: false,
                max_connections: Some(1),
                min_segment_size: Some(4096),
                // Local test server has no certificate (#1172).
                allow_insecure_http: true,
            },
            progress,
            cancel_clone,
        )
        .await
    });

    let first_progress = tokio::task::spawn_blocking(move || {
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("expected progress event")
    })
    .await
    .unwrap();
    assert_eq!(first_progress.2, DownloadPhase::Downloading);

    cancel.cancel();
    let result = task.await.unwrap();
    assert!(matches!(result, Err(DownloadError::Cancelled)));
    assert!(
        tokio::fs::metadata(&temp_path).await.is_err(),
        "temp file should be removed after cancellation"
    );
    assert!(
        tokio::fs::metadata(dir.path().join("cancel.bin"))
            .await
            .is_err(),
        "final destination should not exist after cancellation"
    );
    assert!(
        tokio::fs::metadata(dir.path().join("cancel-meta"))
            .await
            .is_err(),
        "metadata directory should be removed after cancellation"
    );
}

#[test]
fn percentage_rounds_to_two_decimals() {
    assert_eq!(percentage(1, Some(3)), Some(33.33));
    assert_eq!(percentage(2, Some(3)), Some(66.67));
    assert_eq!(percentage(0, None), None);
}

/// #1172: the engine is https-only by default. Downloaded artifacts are
/// written to disk and often executed or linked, and a checksum is
/// optional at most call sites, so the transport is frequently the only
/// integrity guarantee there is.
///
/// This drives the real `download_to_path` against a real plaintext
/// server, so it fails if the builder ever stops applying the policy —
/// asserting on the flag alone would not.
#[tokio::test]
async fn plaintext_http_is_refused_unless_explicitly_allowed() {
    let server = TestServer::start(TestServerConfig {
        body: Arc::new(vec![0u8; 128]),
        accept_ranges: false,
        send_content_length: true,
        chunk_size: 0,
        chunk_delay_ms: 0,
        etag: None,
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("payload.bin");
    let metadata_dir = dir.path().join("metadata");

    let refused = download_to_path(
        &server.url,
        &destination,
        &metadata_dir,
        // Default: the secure setting.
        &DownloadOptions::default(),
        Arc::new(|_, _, _| {}),
        CancellationSource::new().token(),
    )
    .await;

    assert!(
        refused.is_err(),
        "an http:// source must be refused by default, got {refused:?}"
    );
    assert!(
        !destination.exists(),
        "nothing may be written when the transport was refused"
    );

    // And the opt-out still works, so the escape hatch for local test
    // servers is real rather than decorative.
    let allowed = download_to_path(
        &server.url,
        &destination,
        &metadata_dir,
        &DownloadOptions {
            allow_insecure_http: true,
            ..DownloadOptions::default()
        },
        Arc::new(|_, _, _| {}),
        CancellationSource::new().token(),
    )
    .await;
    assert!(allowed.is_ok(), "explicit opt-in must still download");
}
