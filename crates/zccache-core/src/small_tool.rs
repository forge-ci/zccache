//! Owner of small-tool diagnostics (#1970).
//!
//! A small tool is a helper zccache runs for its own purposes, such as a
//! compiler identity probe. Its stdout is the caller's data and is never
//! changed here. Its stderr is evidence: this module keeps a bounded, redacted
//! excerpt with the tool's identity, status and duration, and records it in
//! the lifecycle log and tracing. Nothing is written to the client's streams.

use crate::lifecycle;
use std::time::{Duration, Instant};

/// Largest stderr excerpt retained, in bytes. Keeps the end of the stream,
/// where tools usually report why they failed.
pub const STDERR_EXCERPT_LIMIT_BYTES: usize = 4096;

/// Stderr scanned for an excerpt. Bounds redaction work on a noisy helper.
const STDERR_SCAN_LIMIT_BYTES: usize = 64 * 1024;

const REDACTED: &str = "<redacted>";

/// Lower-case substrings that mark a name as carrying a secret value.
pub const SENSITIVE_NAMES: &[&str] = &[
    "token",
    "password",
    "passwd",
    "secret",
    "api_key",
    "apikey",
    "authorization",
    "credential",
    "private_key",
    "access_key",
    "bearer",
];

/// What one small-tool run left behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmallToolDiagnostic {
    /// Redacted tool identity.
    pub tool: String,
    /// Native exit code; `None` when the tool did not report one.
    pub exit_code: Option<i32>,
    pub duration_ns: u64,
    /// Bounded, redacted tail of stderr.
    pub stderr_excerpt: String,
    /// Stderr size before bounding.
    pub stderr_bytes: usize,
    /// Why the run produced no exit status, redacted.
    pub error: Option<String>,
}

impl SmallToolDiagnostic {
    pub fn completed(
        tool: &str,
        exit_code: Option<i32>,
        duration: Duration,
        stderr: &[u8],
    ) -> Self {
        Self {
            tool: redact_text(tool),
            exit_code,
            duration_ns: duration_ns(duration),
            stderr_excerpt: stderr_excerpt(stderr),
            stderr_bytes: stderr.len(),
            error: None,
        }
    }

    pub fn failed_to_run(tool: &str, duration: Duration, error: &str) -> Self {
        Self {
            tool: redact_text(tool),
            exit_code: None,
            duration_ns: duration_ns(duration),
            stderr_excerpt: String::new(),
            stderr_bytes: 0,
            error: Some(redact_text(error)),
        }
    }

    /// A clean, silent run leaves nothing worth keeping.
    pub fn is_noteworthy(&self) -> bool {
        self.exit_code != Some(0) || !self.stderr_excerpt.is_empty()
    }

    /// One-line failure description for callers that report upward.
    pub fn summary(&self) -> String {
        let status = match (&self.error, self.exit_code) {
            (Some(error), _) => format!("did not run: {error}"),
            (None, Some(code)) => format!("exited with code {code}"),
            (None, None) => "exited without a code".to_string(),
        };
        if self.stderr_excerpt.is_empty() {
            format!("{}: {status}", self.tool)
        } else {
            format!("{}: {status}: {}", self.tool, self.stderr_excerpt)
        }
    }

    /// Record the diagnostic if it is noteworthy. Best-effort, like every
    /// lifecycle event, and never touches stdout or the client's stderr.
    pub fn record(&self) {
        if !self.is_noteworthy() {
            return;
        }
        if self.exit_code == Some(0) {
            tracing::debug!(
                event = lifecycle::EVENT_SMALL_TOOL_DIAGNOSTIC,
                "{}",
                self.summary()
            );
        } else {
            tracing::warn!(
                event = lifecycle::EVENT_SMALL_TOOL_DIAGNOSTIC,
                "{}",
                self.summary()
            );
        }
        lifecycle::write_event(
            lifecycle::EVENT_SMALL_TOOL_DIAGNOSTIC,
            serde_json::json!({
                "tool": self.tool,
                "exit_code": self.exit_code,
                "duration_ns": self.duration_ns,
                "stderr_excerpt": self.stderr_excerpt,
                "stderr_bytes": self.stderr_bytes,
                "error": self.error,
            }),
        );
    }
}

/// Run a captured small tool under `bound` and record what it left behind.
pub fn output_for_recorded(
    tool: &str,
    spec: kernal_api::SpawnSpec,
    bound: Duration,
) -> Result<kernal_api::BoundedProcessOutput, kernal_api::BoundedProcessError> {
    let started = Instant::now();
    let result = crate::wait::output_for(spec, bound);
    let diagnostic = match &result {
        Ok(output) => SmallToolDiagnostic::completed(
            tool,
            Some(output.exit.raw_code()),
            started.elapsed(),
            &output.stderr,
        ),
        Err(error) => {
            SmallToolDiagnostic::failed_to_run(tool, started.elapsed(), &error.to_string())
        }
    };
    diagnostic.record();
    result
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn stderr_excerpt(stderr: &[u8]) -> String {
    let scan_start = stderr.len().saturating_sub(STDERR_SCAN_LIMIT_BYTES);
    let mut scanned = &stderr[scan_start..];
    if scan_start > 0 {
        // A sliced first line could have lost the name that marks its value
        // as secret, so it cannot be redacted reliably.
        let next_line = scanned.iter().position(|byte| *byte == b'\n');
        scanned = next_line.map_or(&[], |index| &scanned[index + 1..]);
    }
    let redacted = redact_text(String::from_utf8_lossy(scanned).trim());
    if redacted.len() <= STDERR_EXCERPT_LIMIT_BYTES {
        return redacted;
    }
    // Redact before truncating: truncation can only remove text.
    let mut start = redacted.len() - STDERR_EXCERPT_LIMIT_BYTES;
    while !redacted.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &redacted[start..])
}

/// Mask values that follow a sensitive name, and URL credentials.
pub fn redact_text(text: &str) -> String {
    text.lines().map(redact_line).collect::<Vec<_>>().join("\n")
}

fn redact_line(line: &str) -> String {
    let mut redact_next = false;
    let mut words = Vec::new();
    for word in line.split_whitespace() {
        let sensitive = is_sensitive(word);
        if redact_next {
            words.push(REDACTED.to_string());
            // `Authorization: Bearer <value>`: the masked word was a name too.
            redact_next = sensitive;
        } else if sensitive {
            match word.find(['=', ':']) {
                Some(index) if index + 1 < word.len() => {
                    words.push(format!("{}{REDACTED}", &word[..=index]));
                }
                _ => {
                    words.push(word.to_string());
                    redact_next = true;
                }
            }
        } else {
            words.push(redact_url_credentials(word));
        }
    }
    words.join(" ")
}

fn is_sensitive(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    SENSITIVE_NAMES.iter().any(|name| lower.contains(name))
}

fn redact_url_credentials(word: &str) -> String {
    if let Some((scheme, rest)) = word.split_once("://") {
        let authority_end = rest.find('/').unwrap_or(rest.len());
        if let Some(at) = rest[..authority_end].rfind('@') {
            return format!("{scheme}://{REDACTED}{}", &rest[at..]);
        }
    }
    word.to_string()
}

#[cfg(test)]
#[path = "small_tool_tests.rs"]
mod tests;
