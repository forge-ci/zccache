//! Product HTTP boundary (#1968): every wait on the network has a budget.
//!
//! Three separate clocks, because a total deadline alone would either hang on a
//! dead peer or kill a healthy slow download:
//!
//! - **Connect** ([`CONNECT_BUDGET`]) covers DNS, TCP and TLS for one connection.
//! - **Headers** ([`HttpPolicy::headers`]) runs from send to response headers,
//!   redirects included.
//! - **Stall** ([`HttpPolicy::stall`]) restarts on every body chunk, so a slow
//!   transfer that keeps moving never expires. Only time spent waiting on the
//!   socket counts: a slow local disk cannot trip it.
//!
//! [`HttpPolicy::total`] additionally caps finite control-plane exchanges. Large
//! downloads leave it unset and are bounded by progress, plus the caller's
//! cancellation token and whole-command wait.
//!
//! Budget values:
//!
//! - Connect 30 s: zccache has no retry layer, and on Linux 30 s leaves room for
//!   the SYN retransmits at 1, 3, 7 and 15 s plus a TLS handshake on a loaded host.
//! - Headers 120 s and transfer stall 120 s match soldr's asset budgets
//!   (`ASSET_HEADER_TIMEOUT`, `ASSET_IDLE_TIMEOUT`), which are in production for
//!   the same CDNs and the same loaded CI hosts.
//! - Control stall 60 s and total 180 s: control bodies are small, so a minute
//!   without a byte is already a failure and the total only needs to fit one
//!   worst-case header wait plus one stall.

use std::fmt;
use std::future::Future;
use std::time::{Duration, Instant};

use kernal_api::async_engine::{CancellationSource, CancellationToken};
use thiserror::Error;

pub use reqwest::header;
pub use reqwest::{StatusCode, Url};

/// Connect budget for product clients.
pub const CONNECT_BUDGET: Duration = Duration::from_secs(30);

/// Finite API exchanges and probes.
pub const CONTROL: HttpPolicy = HttpPolicy {
    headers: Duration::from_secs(120),
    stall: Duration::from_secs(60),
    total: Some(Duration::from_secs(180)),
};

/// Large transfers: bounded by progress, never by elapsed time.
pub const TRANSFER: HttpPolicy = HttpPolicy {
    headers: Duration::from_secs(120),
    stall: Duration::from_secs(120),
    total: None,
};

/// Slowest upload rate the header budget has to allow for. The server answers
/// only after receiving the body, and reqwest offers no write-progress signal,
/// so a request body extends the header budget at this floor.
const UPLOAD_FLOOR_BYTES_PER_SEC: u64 = 64 * 1024;

const MAX_DIAGNOSTIC_BODY_BYTES: usize = 64 * 1024;
const MAX_TRANSPORT_MESSAGE_CHARS: usize = 512;

/// Mandatory budgets for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpPolicy {
    pub headers: Duration,
    pub stall: Duration,
    pub total: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpPhase {
    Headers,
    BodyStall,
    Total,
}

impl fmt::Display for HttpPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Headers => "response-header",
            Self::BodyStall => "body-stall",
            Self::Total => "total",
        })
    }
}

/// Failures from the HTTP boundary. Messages never carry a query string or
/// userinfo, because either may hold a credential.
#[derive(Debug, Error)]
pub enum HttpError {
    #[error("{phase} deadline of {budget:?} expired for {url}")]
    Deadline {
        phase: HttpPhase,
        budget: Duration,
        url: String,
    },
    #[error("http request cancelled")]
    Cancelled,
    #[error("{0}")]
    Transport(String),
    #[error("response body from {url} exceeds the {limit} byte limit")]
    BodyTooLarge { url: String, limit: usize },
}

impl HttpError {
    fn transport(error: reqwest::Error) -> Self {
        let error = error.without_url();
        let mut message = error.to_string();
        let mut source = std::error::Error::source(&error);
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        Self::Transport(message.chars().take(MAX_TRANSPORT_MESSAGE_CHARS).collect())
    }
}

/// `url` without userinfo, query or fragment, safe for logs and errors.
pub fn redact_url(url: &str) -> String {
    let Ok(mut parsed) = Url::parse(url) else {
        return "<unparsable url>".to_string();
    };
    // Both setters refuse only for URLs that cannot have userinfo at all.
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_query(None);
    parsed.set_fragment(None);
    parsed.to_string()
}

/// A token for a caller with no cancellation domain of its own.
pub fn uncancelled() -> CancellationToken {
    CancellationSource::new().token()
}

#[derive(Debug, Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
}

impl HttpClient {
    /// `https_only` also makes an https to http redirect fail (#1172).
    pub fn new(user_agent: &str, https_only: bool, connect: Duration) -> Result<Self, HttpError> {
        reqwest::Client::builder()
            .user_agent(user_agent)
            .https_only(https_only)
            .connect_timeout(connect)
            .build()
            .map(|inner| Self { inner })
            .map_err(HttpError::transport)
    }

    pub fn get(&self, url: &str, policy: HttpPolicy) -> HttpRequest {
        self.request(reqwest::Method::GET, url, policy)
    }

    pub fn head(&self, url: &str, policy: HttpPolicy) -> HttpRequest {
        self.request(reqwest::Method::HEAD, url, policy)
    }

    pub fn post(&self, url: &str, policy: HttpPolicy) -> HttpRequest {
        self.request(reqwest::Method::POST, url, policy)
    }

    pub fn patch(&self, url: &str, policy: HttpPolicy) -> HttpRequest {
        self.request(reqwest::Method::PATCH, url, policy)
    }

    fn request(&self, method: reqwest::Method, url: &str, policy: HttpPolicy) -> HttpRequest {
        HttpRequest {
            inner: self.inner.request(method, url),
            url: redact_url(url),
            policy,
        }
    }
}

#[must_use = "a request does nothing until sent"]
pub struct HttpRequest {
    inner: reqwest::RequestBuilder,
    url: String,
    policy: HttpPolicy,
}

impl HttpRequest {
    pub fn header(mut self, name: header::HeaderName, value: &str) -> Self {
        self.inner = self.inner.header(name, value);
        self
    }

    /// Adds `value` marked sensitive, so it stays out of debug output.
    pub fn secret_header(mut self, name: header::HeaderName, value: &str) -> Self {
        match header::HeaderValue::from_str(value) {
            Ok(mut value) => {
                value.set_sensitive(true);
                self.inner = self.inner.header(name, value);
            }
            // Passing the raw text on makes reqwest report the invalid value as
            // a send error, and the error does not echo it.
            Err(_) => self.inner = self.inner.header(name, value),
        }
        self
    }

    /// Sets the request body and extends the header budget to cover sending it.
    pub fn body(mut self, body: Vec<u8>) -> Self {
        let seconds = body.len() as u64 / UPLOAD_FLOOR_BYTES_PER_SEC;
        self.policy.headers = self
            .policy
            .headers
            .saturating_add(Duration::from_secs(seconds));
        self.inner = self.inner.body(body);
        self
    }

    /// Sends and waits for response headers within budget.
    pub async fn send(self, cancel: &CancellationToken) -> Result<HttpResponse, HttpError> {
        let Self { inner, url, policy } = self;
        let deadline = policy.total.map(|total| (Instant::now(), total));
        let response = bounded(
            cancel,
            &url,
            HttpPhase::Headers,
            policy.headers,
            deadline,
            inner.send(),
        )
        .await?
        .map_err(HttpError::transport)?;
        Ok(HttpResponse {
            inner: response,
            url,
            stall: policy.stall,
            deadline,
            cancel: cancel.clone(),
        })
    }
}

/// A response whose body reads are bounded and cancellable.
pub struct HttpResponse {
    inner: reqwest::Response,
    url: String,
    stall: Duration,
    deadline: Option<(Instant, Duration)>,
    cancel: CancellationToken,
}

impl HttpResponse {
    pub fn status(&self) -> StatusCode {
        self.inner.status()
    }

    pub fn header(&self, name: header::HeaderName) -> Option<&str> {
        self.inner.headers().get(name)?.to_str().ok()
    }

    pub fn content_length(&self) -> Option<u64> {
        self.inner.content_length()
    }

    /// The next body chunk, or `None` at the end of the body.
    pub async fn next_chunk(&mut self) -> Result<Option<impl AsRef<[u8]>>, HttpError> {
        bounded(
            &self.cancel,
            &self.url,
            HttpPhase::BodyStall,
            self.stall,
            self.deadline,
            self.inner.chunk(),
        )
        .await?
        .map_err(HttpError::transport)
    }

    /// Reads the whole body, whatever its size.
    pub async fn read_to_end(mut self) -> Result<Vec<u8>, HttpError> {
        let mut out = Vec::new();
        while let Some(chunk) = self.next_chunk().await? {
            out.extend_from_slice(chunk.as_ref());
        }
        Ok(out)
    }

    /// Reads a body expected to be small, failing beyond `limit` bytes.
    pub async fn read_limited(mut self, limit: usize) -> Result<Vec<u8>, HttpError> {
        let mut out = Vec::new();
        while let Some(chunk) = self.next_chunk().await? {
            out.extend_from_slice(chunk.as_ref());
            if out.len() > limit {
                return Err(HttpError::BodyTooLarge {
                    url: self.url,
                    limit,
                });
            }
        }
        Ok(out)
    }

    /// Best-effort error body for a diagnostic: bounded, and whatever arrived
    /// before a failure is still returned.
    pub async fn diagnostic_body(mut self) -> String {
        let mut out = Vec::new();
        while out.len() < MAX_DIAGNOSTIC_BODY_BYTES {
            match self.next_chunk().await {
                Ok(Some(chunk)) => out.extend_from_slice(chunk.as_ref()),
                _ => break,
            }
        }
        out.truncate(MAX_DIAGNOSTIC_BODY_BYTES);
        String::from_utf8_lossy(&out).into_owned()
    }
}

/// Awaits `future` for at most `budget`, clamped to what remains of `deadline`.
/// Cancellation wins over a simultaneously ready result; the future wins over a
/// simultaneously expired timer, so a descheduled task never mistakes its own
/// lateness for a stalled peer.
async fn bounded<F: Future>(
    cancel: &CancellationToken,
    url: &str,
    phase: HttpPhase,
    budget: Duration,
    deadline: Option<(Instant, Duration)>,
    future: F,
) -> Result<F::Output, HttpError> {
    let (wait, phase, budget) = match deadline {
        Some((started, total)) if total.saturating_sub(started.elapsed()) < budget => (
            total.saturating_sub(started.elapsed()),
            HttpPhase::Total,
            total,
        ),
        _ => (budget, phase, budget),
    };
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(HttpError::Cancelled),
        outcome = tokio::time::timeout(wait, future) => outcome.map_err(|_| HttpError::Deadline {
            phase,
            budget,
            url: url.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests;
