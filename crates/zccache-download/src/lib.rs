#![allow(clippy::missing_errors_doc)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
use kernal_api::async_engine::CancellationSource;
use kernal_api::async_engine::CancellationToken;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zccache_core::NormalizedPath;

use http::header::{
    ACCEPT_ENCODING, ACCEPT_RANGES, CONTENT_LENGTH, ETAG, IF_RANGE, LAST_MODIFIED, RANGE,
};
use http::{HttpClient, HttpError, HttpPolicy, StatusCode};

pub mod http;

#[cfg(test)]
mod stall_tests;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadOptions {
    pub force: bool,
    pub max_connections: Option<usize>,
    pub min_segment_size: Option<u64>,
    /// Permit plaintext `http://` for this download (#1172).
    ///
    /// The engine is https-only by default: downloaded artifacts are written
    /// to disk and often executed or linked, and a checksum is optional at
    /// most call sites, so the transport is frequently the only integrity
    /// guarantee there is. `https_only` also makes an https→http **redirect**
    /// fail rather than silently downgrade, which is the case a URL check at
    /// the call site would miss.
    ///
    /// The escape hatch exists for local test servers, which have no
    /// certificate. `#[serde(default)]` keeps it false — i.e. secure — for any
    /// request serialized before this field existed.
    #[serde(default)]
    pub allow_insecure_http: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadPhase {
    Pending,
    Downloading,
    Finalizing,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadStatus {
    pub phase: DownloadPhase,
    pub total_bytes: Option<u64>,
    pub downloaded_bytes: u64,
    pub percentage: Option<f32>,
    pub active_clients: u32,
    pub destination: NormalizedPath,
    pub source_url: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadDaemonStatus {
    pub version: String,
    pub active_downloads: u64,
    pub connected_clients: u64,
    pub uptime_secs: u64,
    pub endpoint: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadAttachResult {
    pub download_id: String,
    pub initiator: bool,
    pub status: DownloadStatus,
}

#[derive(Debug, Clone)]
struct Probe {
    total_bytes: Option<u64>,
    accept_ranges: bool,
    validator: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub max_connections: usize,
    pub min_segment_size: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self {
            max_connections: (cpus * 2).min(16),
            min_segment_size: 8 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("http error: {0}")]
    Http(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("download cancelled")]
    Cancelled,
    #[error("remote refused ranged download")]
    RangeUnsupported,
}

impl From<HttpError> for DownloadError {
    fn from(error: HttpError) -> Self {
        match error {
            HttpError::Cancelled => Self::Cancelled,
            other => Self::Http(other.to_string()),
        }
    }
}

/// The budgets one download runs under; see [`http`] for the chosen values.
#[derive(Debug, Clone, Copy)]
struct Budgets {
    connect: Duration,
    probe: HttpPolicy,
    body: HttpPolicy,
}

pub type ProgressCallback = Arc<dyn Fn(u64, Option<u64>, DownloadPhase) + Send + Sync>;

struct SegmentedDownload<'a> {
    client: &'a HttpClient,
    url: &'a str,
    policy: HttpPolicy,
    temp_path: &'a Path,
    metadata_dir: &'a Path,
    total: u64,
    max_connections: usize,
    min_segment_size: u64,
    validator: Option<String>,
    progress: ProgressCallback,
    cancel_token: CancellationToken,
}

struct SegmentDownload<'a> {
    client: &'a HttpClient,
    url: &'a str,
    policy: HttpPolicy,
    segment_path: &'a Path,
    start: u64,
    end: u64,
    validator: Option<String>,
    progress: Arc<dyn Fn(u64) + Send + Sync>,
    cancel_token: CancellationToken,
}

pub async fn download_to_path(
    url: &str,
    destination: &Path,
    metadata_dir: &Path,
    options: &DownloadOptions,
    progress: ProgressCallback,
    cancel_token: CancellationToken,
) -> Result<Option<u64>, DownloadError> {
    download_with_budgets(
        url,
        destination,
        metadata_dir,
        options,
        progress,
        cancel_token,
        Budgets {
            connect: http::CONNECT_BUDGET,
            probe: http::CONTROL,
            body: http::TRANSFER,
        },
    )
    .await
}

async fn download_with_budgets(
    url: &str,
    destination: &Path,
    metadata_dir: &Path,
    options: &DownloadOptions,
    progress: ProgressCallback,
    cancel_token: CancellationToken,
    budgets: Budgets,
) -> Result<Option<u64>, DownloadError> {
    // #1172: https-only unless the caller explicitly opted out. This also
    // rejects an https→http redirect, which a URL check at the call site would
    // not catch.
    let client = HttpClient::new(
        &format!("zccache-download/{}", zccache_core::VERSION),
        !options.allow_insecure_http,
        budgets.connect,
    )?;

    let probe = probe_remote(&client, url, budgets.probe, &cancel_token).await?;
    let total = probe.total_bytes;
    let engine = EngineConfig::default();
    let max_connections = options
        .max_connections
        .unwrap_or(engine.max_connections)
        .max(1);
    let min_segment_size = options
        .min_segment_size
        .unwrap_or(engine.min_segment_size)
        .max(1);

    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::create_dir_all(metadata_dir).await?;

    let temp_path = destination.with_extension(format!(
        "{}part",
        destination
            .extension()
            .map(|ext| format!("{}.", ext.to_string_lossy()))
            .unwrap_or_default()
    ));

    if options.force {
        let _ = tokio::fs::remove_file(&temp_path).await;
    }

    progress(0, total, DownloadPhase::Downloading);

    let result = if should_use_segments(
        total,
        probe.accept_ranges,
        max_connections,
        min_segment_size,
    ) {
        #[expect(
            clippy::expect_used,
            reason = "should_use_segments(total, ...) returns false when total is None; reaching this branch implies Some(_)"
        )]
        let total_bytes = total.expect("segment use requires total");
        download_segmented(SegmentedDownload {
            client: &client,
            url,
            policy: budgets.body,
            temp_path: &temp_path,
            metadata_dir,
            total: total_bytes,
            max_connections,
            min_segment_size,
            validator: probe.validator,
            progress: progress.clone(),
            cancel_token: cancel_token.clone(),
        })
        .await
    } else {
        download_single(
            &client,
            url,
            budgets.body,
            &temp_path,
            total,
            progress.clone(),
            cancel_token.clone(),
        )
        .await
    };

    match result {
        Ok(bytes) => {
            progress(bytes, total.or(Some(bytes)), DownloadPhase::Finalizing);
            if destination.exists() {
                let _ = tokio::fs::remove_file(destination).await;
            }
            tokio::fs::rename(&temp_path, destination).await?;
            progress(bytes, total.or(Some(bytes)), DownloadPhase::Completed);
            Ok(total.or(Some(bytes)))
        }
        Err(DownloadError::Cancelled) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            cleanup_segment_dir(metadata_dir).await;
            progress(0, total, DownloadPhase::Cancelled);
            Err(DownloadError::Cancelled)
        }
        Err(err) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            cleanup_segment_dir(metadata_dir).await;
            Err(err)
        }
    }
}

async fn cleanup_segment_dir(metadata_dir: &Path) {
    let _ = tokio::fs::remove_dir_all(metadata_dir).await;
}

fn should_use_segments(
    total_bytes: Option<u64>,
    accept_ranges: bool,
    max_connections: usize,
    min_segment_size: u64,
) -> bool {
    match total_bytes {
        Some(total) => accept_ranges && max_connections > 1 && total >= min_segment_size * 2,
        None => false,
    }
}

async fn probe_remote(
    client: &HttpClient,
    url: &str,
    policy: HttpPolicy,
    cancel_token: &CancellationToken,
) -> Result<Probe, DownloadError> {
    let head = client
        .head(url, policy)
        .header(ACCEPT_ENCODING, "identity")
        .send(cancel_token)
        .await?;
    if head.status().is_success() {
        let total_bytes = head
            .header(CONTENT_LENGTH)
            .and_then(|v| v.parse::<u64>().ok());
        let accept_ranges = head
            .header(ACCEPT_RANGES)
            .map(|v| v.eq_ignore_ascii_case("bytes"))
            .unwrap_or(false);
        let validator = head
            .header(ETAG)
            .or_else(|| head.header(LAST_MODIFIED))
            .map(ToOwned::to_owned);
        return Ok(Probe {
            total_bytes,
            accept_ranges,
            validator,
        });
    }

    let get = client
        .get(url, policy)
        .header(ACCEPT_ENCODING, "identity")
        .header(RANGE, "bytes=0-0")
        .send(cancel_token)
        .await?;
    let total_bytes = get.content_length().map(|len| len.max(1));
    let accept_ranges = get.status() == StatusCode::PARTIAL_CONTENT;
    let validator = get
        .header(ETAG)
        .or_else(|| get.header(LAST_MODIFIED))
        .map(ToOwned::to_owned);
    Ok(Probe {
        total_bytes,
        accept_ranges,
        validator,
    })
}

async fn download_single(
    client: &HttpClient,
    url: &str,
    policy: HttpPolicy,
    temp_path: &Path,
    total: Option<u64>,
    progress: ProgressCallback,
    cancel_token: CancellationToken,
) -> Result<u64, DownloadError> {
    let mut response = client
        .get(url, policy)
        .header(ACCEPT_ENCODING, "identity")
        .send(&cancel_token)
        .await?;
    if !response.status().is_success() {
        return Err(DownloadError::Http(format!(
            "unexpected status {} for {}",
            response.status(),
            http::redact_url(url)
        )));
    }

    let mut file = tokio::fs::File::create(temp_path).await?;
    let mut downloaded = 0u64;
    while let Some(chunk) = response.next_chunk().await? {
        let chunk = chunk.as_ref();
        file.write_all(chunk).await?;
        downloaded += chunk.len() as u64;
        progress(downloaded, total, DownloadPhase::Downloading);
    }
    file.flush().await?;
    Ok(downloaded)
}

async fn download_segmented(request: SegmentedDownload<'_>) -> Result<u64, DownloadError> {
    let SegmentedDownload {
        client,
        url,
        policy,
        temp_path,
        metadata_dir,
        total,
        max_connections,
        min_segment_size,
        validator,
        progress,
        cancel_token,
    } = request;

    let segments = build_segments(total, max_connections, min_segment_size);
    let progress_total = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut tasks = Vec::with_capacity(segments.len());

    for (index, (start, end)) in segments.iter().copied().enumerate() {
        let client = client.clone();
        let url = url.to_string();
        let segment_path = metadata_dir.join(format!("segment-{index:03}.part"));
        let validator = validator.clone();
        let progress = Arc::clone(&progress);
        let progress_total = Arc::clone(&progress_total);
        let cancel_token = cancel_token.clone();
        tasks.push(tokio::spawn(async move {
            let downloaded = download_segment(SegmentDownload {
                client: &client,
                url: &url,
                policy,
                segment_path: &segment_path,
                start,
                end,
                validator,
                progress: Arc::new(move |delta| {
                    let total_downloaded = progress_total
                        .fetch_add(delta, std::sync::atomic::Ordering::Relaxed)
                        + delta;
                    progress(total_downloaded, Some(total), DownloadPhase::Downloading);
                }),
                cancel_token,
            })
            .await?;
            Ok::<_, DownloadError>((index, segment_path, downloaded))
        }));
    }

    let mut segment_paths = Vec::with_capacity(tasks.len());
    for task in tasks {
        let (index, path, _bytes) = task
            .await
            .map_err(|e| DownloadError::Http(e.to_string()))??;
        segment_paths.push((index, path));
    }
    segment_paths.sort_by_key(|(index, _)| *index);

    let mut out = tokio::fs::File::create(temp_path).await?;
    for (_, path) in segment_paths {
        if cancel_token.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let mut segment = tokio::fs::File::open(&path).await?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = segment.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).await?;
        }
        let _ = tokio::fs::remove_file(&path).await;
    }
    out.flush().await?;
    cleanup_segment_dir(metadata_dir).await;
    Ok(total)
}

fn build_segments(total: u64, max_connections: usize, min_segment_size: u64) -> Vec<(u64, u64)> {
    let max_segments = total.div_ceil(min_segment_size) as usize;
    let segments = max_segments.max(1).min(max_connections.max(1));
    let base = total / segments as u64;
    let remainder = total % segments as u64;

    let mut out = Vec::with_capacity(segments);
    let mut start = 0u64;
    for i in 0..segments {
        let extra = if (i as u64) < remainder { 1 } else { 0 };
        let len = base + extra;
        let end = start + len - 1;
        out.push((start, end));
        start = end + 1;
    }
    out
}

async fn download_segment(request: SegmentDownload<'_>) -> Result<u64, DownloadError> {
    let SegmentDownload {
        client,
        url,
        policy,
        segment_path,
        start,
        end,
        validator,
        progress,
        cancel_token,
    } = request;

    let mut request = client
        .get(url, policy)
        .header(ACCEPT_ENCODING, "identity")
        .header(RANGE, &format!("bytes={start}-{end}"));
    if let Some(value) = validator.as_deref() {
        request = request.header(IF_RANGE, value);
    }

    let mut response = request.send(&cancel_token).await?;
    if response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(DownloadError::RangeUnsupported);
    }

    let mut file = tokio::fs::File::create(segment_path).await?;
    let mut downloaded = 0u64;
    while let Some(chunk) = response.next_chunk().await? {
        let chunk = chunk.as_ref();
        file.write_all(chunk).await?;
        downloaded += chunk.len() as u64;
        progress(chunk.len() as u64);
    }
    file.flush().await?;
    Ok(downloaded)
}

pub fn stable_download_id(path: &Path) -> String {
    let key = zccache_core::normalize_for_key(path);
    kernal_api::hash::blake3_bytes(key.as_bytes())
        .to_hex()
        .to_string()
}

pub fn canonical_destination(path: &Path) -> Result<NormalizedPath, std::io::Error> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let filename = absolute.file_name().map(ToOwned::to_owned).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "destination must include a file name",
        )
    })?;
    let parent = absolute.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let canonical_parent = std::fs::canonicalize(parent)?;
    Ok(NormalizedPath::new(canonical_parent.join(filename)))
}

pub fn percentage(downloaded: u64, total: Option<u64>) -> Option<f32> {
    total.and_then(|t| {
        if t == 0 {
            None
        } else {
            Some(((downloaded as f64 * 100.0 / t as f64) * 100.0).round() as f32 / 100.0)
        }
    })
}

pub fn uptime_secs(start: std::time::Instant) -> u64 {
    Duration::from_secs(start.elapsed().as_secs()).as_secs()
}

use thiserror::Error;

#[cfg(test)]
mod tests;
