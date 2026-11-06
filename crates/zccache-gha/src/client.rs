use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zccache_download::http::header::{ACCEPT, AUTHORIZATION, CONTENT_RANGE, CONTENT_TYPE};
use zccache_download::http::{
    self, HttpClient, HttpError, HttpPolicy, HttpRequest, HttpResponse, Url,
};

/// Errors from the GitHub Actions cache client.
#[derive(Error, Debug)]
pub enum GhaError {
    #[error("GHA cache API not available (ACTIONS_CACHE_URL or ACTIONS_RUNTIME_TOKEN not set)")]
    NotAvailable,
    #[error("HTTP error: {0}")]
    Http(#[from] HttpError),
    #[error("invalid API response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("API error: {status} {body}")]
    Api { status: u16, body: String },
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Cache not found for key: {0}")]
    NotFound(String),
    #[error("invalid cache URL: {0}")]
    InvalidUrl(String),
}

/// Client for the GitHub Actions Cache API.
///
/// Reads `ACTIONS_CACHE_URL` and `ACTIONS_RUNTIME_TOKEN` from the environment
/// (both are set automatically by the GHA runner).
pub struct GhaCache {
    client: HttpClient,
    base_url: String,
    token: String,
    /// Reserve, commit and restore lookups.
    control: HttpPolicy,
    /// Blob upload and download.
    transfer: HttpPolicy,
}

impl std::fmt::Debug for GhaCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GhaCache")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct ReserveCacheRequest {
    key: String,
    version: String,
}

#[derive(Deserialize)]
struct ReserveCacheResponse {
    #[serde(rename = "cacheId")]
    cache_id: i64,
}

#[derive(Serialize)]
struct CommitCacheRequest {
    size: u64,
}

#[derive(Deserialize)]
struct RestoreCacheResponse {
    #[serde(rename = "archiveLocation")]
    archive_location: Option<String>,
    #[serde(rename = "cacheKey")]
    #[allow(dead_code)]
    cache_key: Option<String>,
}

/// API version header value used by the GHA cache REST API.
const API_VERSION: &str = "application/json;api-version=6.0-preview.1";

/// API responses are small JSON documents; anything larger is not one.
const MAX_API_RESPONSE_BYTES: usize = 1024 * 1024;

impl GhaCache {
    /// Create a new GHA cache client from environment variables.
    ///
    /// Returns `Err(GhaError::NotAvailable)` when not running inside GitHub
    /// Actions (i.e., the required env vars are missing).
    pub fn from_env() -> Result<Self, GhaError> {
        let base_url = std::env::var("ACTIONS_CACHE_URL").map_err(|_| GhaError::NotAvailable)?;
        let token = std::env::var("ACTIONS_RUNTIME_TOKEN").map_err(|_| GhaError::NotAvailable)?;

        Self::with_budgets(
            &base_url,
            token,
            http::CONNECT_BUDGET,
            http::CONTROL,
            http::TRANSFER,
        )
    }

    fn with_budgets(
        base_url: &str,
        token: String,
        connect: Duration,
        control: HttpPolicy,
        transfer: HttpPolicy,
    ) -> Result<Self, GhaError> {
        Ok(Self {
            client: HttpClient::new("zccache", false, connect)?,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            control,
            transfer,
        })
    }

    /// Check whether GHA cache env vars are present.
    pub fn is_available() -> bool {
        std::env::var("ACTIONS_CACHE_URL").is_ok() && std::env::var("ACTIONS_RUNTIME_TOKEN").is_ok()
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}/_apis/artifactcache/{}", self.base_url, path)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }

    fn authorized_json<T: Serialize>(&self, url: &str, body: &T) -> Result<HttpRequest, GhaError> {
        Ok(self
            .client
            .post(url, self.control)
            .secret_header(AUTHORIZATION, &self.auth_header())
            .header(ACCEPT, API_VERSION)
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(body)?))
    }

    async fn send(request: HttpRequest) -> Result<HttpResponse, GhaError> {
        Ok(request.send(&http::uncancelled()).await?)
    }

    async fn api_error(response: HttpResponse) -> GhaError {
        GhaError::Api {
            status: response.status().as_u16(),
            body: response.diagnostic_body().await,
        }
    }

    async fn read_json<T: serde::de::DeserializeOwned>(
        response: HttpResponse,
    ) -> Result<T, GhaError> {
        let body = response.read_limited(MAX_API_RESPONSE_BYTES).await?;
        Ok(serde_json::from_slice(&body)?)
    }

    /// Build the cache-restore URL, percent-encoding `key` and `version`.
    ///
    /// The values come from the caller (argv on the CLI path) and may contain
    /// `#`, `&`, spaces, or any other reserved character, so they are encoded
    /// as query parameters rather than interpolated into the query string.
    fn restore_url(&self, key: &str, version: &str) -> Result<String, GhaError> {
        let base = format!("{}/_apis/artifactcache/cache", self.base_url);
        let mut url = Url::parse(&base).map_err(|e| GhaError::InvalidUrl(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("keys", key)
            .append_pair("version", version);
        Ok(url.to_string())
    }

    /// Compute a deterministic version hash from a set of path strings.
    ///
    /// The GHA cache API requires a `version` field that differentiates
    /// caches that share the same key but cover different paths.
    pub fn version_hash(paths: &[&str]) -> String {
        let mut hasher = Sha256::new();
        for p in paths {
            hasher.update(p.as_bytes());
            hasher.update(b"|");
        }
        format!("{:x}", hasher.finalize())
    }

    /// Save a blob to the GHA cache under the given key and version.
    ///
    /// The three-step protocol is: reserve -> upload -> commit.
    /// If the cache key already exists (HTTP 409) the call succeeds silently.
    pub async fn save(&self, key: &str, version: &str, data: &[u8]) -> Result<(), GhaError> {
        // Step 1: Reserve a cache entry.
        let reserve_url = self.api_url("caches");
        let resp = Self::send(self.authorized_json(
            &reserve_url,
            &ReserveCacheRequest {
                key: key.to_string(),
                version: version.to_string(),
            },
        )?)
        .await?;

        if !resp.status().is_success() {
            // 409 Conflict = cache already exists, not an error.
            if resp.status().as_u16() == 409 {
                tracing::info!("cache already exists for key: {key}");
                return Ok(());
            }
            return Err(Self::api_error(resp).await);
        }

        let reserve: ReserveCacheResponse = Self::read_json(resp).await?;
        let cache_id = reserve.cache_id;

        // Step 2: Upload the data in a single chunk.
        let upload_url = self.api_url(&format!("caches/{cache_id}"));
        let len = data.len();
        let content_range = if len == 0 {
            "bytes */*".to_string()
        } else {
            format!("bytes 0-{}/{len}", len - 1)
        };
        let resp = Self::send(
            self.client
                .patch(&upload_url, self.transfer)
                .secret_header(AUTHORIZATION, &self.auth_header())
                .header(CONTENT_TYPE, "application/octet-stream")
                .header(CONTENT_RANGE, &content_range)
                .body(data.to_vec()),
        )
        .await?;

        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }

        // Step 3: Commit (finalize) the cache entry.
        let commit_url = self.api_url(&format!("caches/{cache_id}"));
        let resp = Self::send(
            self.authorized_json(&commit_url, &CommitCacheRequest { size: len as u64 })?,
        )
        .await?;

        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }

        tracing::info!("saved {len} bytes to GHA cache key: {key}");
        Ok(())
    }

    /// Restore a blob from the GHA cache. Returns `None` if no entry was found.
    pub async fn restore(&self, key: &str, version: &str) -> Result<Option<Vec<u8>>, GhaError> {
        let url = self.restore_url(key, version)?;
        let resp = Self::send(
            self.client
                .get(&url, self.control)
                .secret_header(AUTHORIZATION, &self.auth_header())
                .header(ACCEPT, API_VERSION),
        )
        .await?;

        // 204 No Content = cache miss.
        if resp.status().as_u16() == 204 {
            return Ok(None);
        }

        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }

        let result: RestoreCacheResponse = Self::read_json(resp).await?;
        let location = match result.archive_location {
            Some(loc) => loc,
            None => return Ok(None),
        };

        // Download the blob from the archive location. A failure status must
        // not be mistaken for the blob: its body is an error document.
        let resp = Self::send(self.client.get(&location, self.transfer)).await?;
        if !resp.status().is_success() {
            return Err(Self::api_error(resp).await);
        }
        let data = resp.read_to_end().await?;

        tracing::info!("restored {} bytes from GHA cache key: {key}", data.len());
        Ok(Some(data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const SHORT: HttpPolicy = HttpPolicy {
        headers: Duration::from_millis(300),
        stall: Duration::from_millis(300),
        total: None,
    };

    /// Answers the first `replies.len()` requests, then stays silent.
    async fn scripted_server(replies: &'static [&'static str]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 64 * 1024];
            let mut replies = replies.iter();
            while stream.read(&mut buf).await.unwrap_or(0) > 0 {
                if let Some(reply) = replies.next() {
                    let _ = stream.write_all(reply.as_bytes()).await;
                }
            }
        });
        format!("http://{addr}")
    }

    fn cache(url: &str) -> GhaCache {
        let connect = Duration::from_secs(5);
        GhaCache::with_budgets(url, "TOKEN".to_string(), connect, SHORT, SHORT).unwrap()
    }

    fn assert_expired<T: std::fmt::Debug>(result: Result<T, GhaError>, phase: &str) {
        match result {
            Err(GhaError::Http(HttpError::Deadline { phase: actual, .. })) => {
                assert_eq!(actual.to_string(), phase);
            }
            other => panic!("expected a {phase} expiry, got {other:?}"),
        }
    }

    async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(30), future)
            .await
            .expect("the budget did not fire")
    }

    const RESERVED: &str = "HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n{\"cacheId\":1}";

    #[tokio::test]
    async fn restore_lookup_expires_on_a_silent_server() {
        let url = scripted_server(&[]).await;
        assert_expired(
            within(cache(&url).restore("k", "v")).await,
            "response-header",
        );
    }

    #[tokio::test]
    async fn reserve_expires_on_a_silent_server() {
        let url = scripted_server(&[]).await;
        assert_expired(
            within(cache(&url).save("k", "v", b"x")).await,
            "response-header",
        );
    }

    #[tokio::test]
    async fn upload_expires_when_the_server_never_answers() {
        let url = scripted_server(&[RESERVED]).await;
        assert_expired(
            within(cache(&url).save("k", "v", b"x")).await,
            "response-header",
        );
    }

    #[tokio::test]
    async fn stalled_control_body_expires() {
        let url = scripted_server(&["HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n{"]).await;
        assert_expired(within(cache(&url).restore("k", "v")).await, "body-stall");
    }

    #[test]
    fn debug_output_omits_the_runtime_token() {
        let rendered = format!("{:?}", cache("http://127.0.0.1:1"));
        assert!(!rendered.contains("TOKEN"), "{rendered}");
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        old_cache_url: Option<String>,
        old_runtime_token: Option<String>,
    }

    impl EnvGuard {
        fn new() -> Self {
            Self {
                _lock: env_lock().lock().unwrap(),
                old_cache_url: std::env::var("ACTIONS_CACHE_URL").ok(),
                old_runtime_token: std::env::var("ACTIONS_RUNTIME_TOKEN").ok(),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.old_cache_url {
                Some(value) => std::env::set_var("ACTIONS_CACHE_URL", value),
                None => std::env::remove_var("ACTIONS_CACHE_URL"),
            }
            match &self.old_runtime_token {
                Some(value) => std::env::set_var("ACTIONS_RUNTIME_TOKEN", value),
                None => std::env::remove_var("ACTIONS_RUNTIME_TOKEN"),
            }
        }
    }

    #[test]
    fn is_available_returns_false_without_env_vars() {
        let _guard = EnvGuard::new();
        // Clear env vars in case they happen to be set.
        std::env::remove_var("ACTIONS_CACHE_URL");
        std::env::remove_var("ACTIONS_RUNTIME_TOKEN");
        assert!(!GhaCache::is_available());
    }

    #[test]
    fn version_hash_is_deterministic() {
        let h1 = GhaCache::version_hash(&["a", "b", "c"]);
        let h2 = GhaCache::version_hash(&["a", "b", "c"]);
        assert_eq!(h1, h2);
        assert!(!h1.is_empty());
    }

    #[test]
    fn version_hash_differs_for_different_inputs() {
        let h1 = GhaCache::version_hash(&["a", "b"]);
        let h2 = GhaCache::version_hash(&["a", "c"]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn from_env_returns_not_available_without_env_vars() {
        let _guard = EnvGuard::new();
        std::env::remove_var("ACTIONS_CACHE_URL");
        std::env::remove_var("ACTIONS_RUNTIME_TOKEN");
        let err = GhaCache::from_env().unwrap_err();
        assert!(
            matches!(err, GhaError::NotAvailable),
            "expected NotAvailable, got: {err}"
        );
    }

    #[test]
    fn from_env_returns_not_available_with_partial_env() {
        let _guard = EnvGuard::new();
        // Only one of the two vars set.
        std::env::set_var("ACTIONS_CACHE_URL", "https://example.com");
        std::env::remove_var("ACTIONS_RUNTIME_TOKEN");
        let err = GhaCache::from_env().unwrap_err();
        assert!(matches!(err, GhaError::NotAvailable));
    }

    #[test]
    fn from_env_succeeds_with_both_env_vars() {
        let _guard = EnvGuard::new();
        std::env::set_var("ACTIONS_CACHE_URL", "https://example.com/cache/");
        std::env::set_var("ACTIONS_RUNTIME_TOKEN", "test-token");
        let cache = GhaCache::from_env().expect("should succeed with both vars set");
        // Verify trailing slash is stripped.
        assert_eq!(cache.base_url, "https://example.com/cache");
        assert_eq!(cache.token, "test-token");
    }

    /// Build a `GhaCache` from `base`, returning the `EnvGuard` alongside it.
    ///
    /// The guard is returned rather than dropped here so the caller keeps
    /// `env_lock()` held for the whole test body; dropping it at the end of
    /// this helper would let a later env mutation in the same test race with
    /// the other env-var tests in this module.
    fn cache_with_base(base: &str) -> (GhaCache, EnvGuard) {
        let guard = EnvGuard::new();
        std::env::set_var("ACTIONS_CACHE_URL", base);
        std::env::set_var("ACTIONS_RUNTIME_TOKEN", "test-token");
        let cache = GhaCache::from_env().expect("both env vars are set");
        (cache, guard)
    }

    fn query_pairs(url: &str) -> Vec<(String, String)> {
        Url::parse(url)
            .expect("restore URL must parse")
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    #[test]
    fn restore_url_percent_encodes_hash_in_key() {
        let (cache, _guard) = cache_with_base("https://example.com/cache/");
        let url = cache.restore_url("plan#1", "v1").expect("valid base url");
        assert!(!url.contains('#'), "key must not leak a fragment: {url}");
        assert_eq!(
            query_pairs(&url),
            vec![
                ("keys".to_string(), "plan#1".to_string()),
                ("version".to_string(), "v1".to_string()),
            ]
        );
    }

    #[test]
    fn restore_url_percent_encodes_ampersand_in_key() {
        let (cache, _guard) = cache_with_base("https://example.com/cache/");
        let url = cache
            .restore_url("a&version=evil", "v1")
            .expect("valid base url");
        assert_eq!(
            query_pairs(&url),
            vec![
                ("keys".to_string(), "a&version=evil".to_string()),
                ("version".to_string(), "v1".to_string()),
            ]
        );
    }

    #[test]
    fn restore_url_percent_encodes_space_in_key() {
        let (cache, _guard) = cache_with_base("https://example.com/cache/");
        let url = cache.restore_url("a b", "v1").expect("valid base url");
        assert_eq!(
            query_pairs(&url),
            vec![
                ("keys".to_string(), "a b".to_string()),
                ("version".to_string(), "v1".to_string()),
            ]
        );
    }

    #[test]
    fn restore_url_keeps_path_and_both_query_params() {
        let (cache, _guard) = cache_with_base("https://example.com/cache/");
        let url = cache
            .restore_url("linux-target/debug", "abc123")
            .expect("valid base url");
        let parsed = Url::parse(&url).expect("restore URL must parse");
        assert_eq!(parsed.path(), "/cache/_apis/artifactcache/cache");
        assert_eq!(parsed.fragment(), None);
        assert_eq!(
            query_pairs(&url),
            vec![
                ("keys".to_string(), "linux-target/debug".to_string()),
                ("version".to_string(), "abc123".to_string()),
            ]
        );
    }

    #[test]
    fn restore_url_rejects_invalid_base_url() {
        let (cache, _guard) = cache_with_base("not a url");
        let err = cache
            .restore_url("k", "v")
            .expect_err("invalid base url must fail");
        assert!(
            matches!(err, GhaError::InvalidUrl(_)),
            "expected InvalidUrl, got: {err}"
        );
    }
}
