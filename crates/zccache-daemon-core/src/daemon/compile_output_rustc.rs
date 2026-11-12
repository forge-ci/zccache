//! Bounded diagnostic capture with immediate rustc metadata checkpoints.
use super::BoundedCapture;
use std::io;

const CONTROL_BYTES: usize = 64 * 1024;

pub(super) struct StreamCapture {
    capture: BoundedCapture,
    pending: Option<Vec<u8>>,
    oversized_line: bool,
}

impl StreamCapture {
    pub(super) fn new(limit: usize, stream: &'static str, rustc: bool) -> Self {
        Self {
            capture: BoundedCapture::new(limit, stream),
            pending: rustc.then(Vec::new),
            oversized_line: false,
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8]) -> io::Result<Vec<Vec<u8>>> {
        let Some(pending) = self.pending.as_mut() else {
            return Ok(self.capture.push(bytes).into_iter().collect());
        };
        pending.extend_from_slice(bytes);
        let mut emitted = Vec::new();
        while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
            let line = pending.drain(..=end).collect::<Vec<_>>();
            let metadata = !self.oversized_line
                && serde_json::from_slice::<serde_json::Value>(&line).is_ok_and(|value| {
                    value.get("emit").and_then(serde_json::Value::as_str) == Some("metadata")
                        && value
                            .get("artifact")
                            .is_some_and(serde_json::Value::is_string)
                });
            if metadata {
                emitted.push(self.capture.checkpoint(&line)?);
            } else if let Some(bytes) = self.capture.push(&line) {
                emitted.push(bytes);
            }
            self.oversized_line = false;
        }
        // Diagnostics can contain arbitrarily long JSON lines. Only control
        // events need framing; pass large diagnostics into bounded capture.
        if pending.len() > CONTROL_BYTES {
            if let Some(bytes) = self.capture.push(pending) {
                emitted.push(bytes);
            }
            pending.clear();
            self.oversized_line = true;
        }
        Ok(emitted)
    }

    pub(super) fn finish(&mut self) -> Vec<Vec<u8>> {
        let mut emitted = Vec::new();
        if let Some(pending) = self.pending.take() {
            if let Some(bytes) = self.capture.push(&pending) {
                emitted.push(bytes);
            }
        }
        if let Some(bytes) = self.capture.finish() {
            emitted.push(bytes);
        }
        emitted
    }

    pub(super) fn into_bytes(self) -> Vec<u8> {
        self.capture.into_bytes()
    }
}

impl BoundedCapture {
    fn checkpoint(&mut self, metadata: &[u8]) -> io::Result<Vec<u8>> {
        let remaining = self.limit.saturating_sub(self.retained_diagnostics);
        // Retain some context before metadata and reserve the rest for errors
        // from later codegen/linking. Already emitted bytes cannot be evicted.
        let keep = self.tail.len().min(remaining / 2);
        let dropped = self.total > self.retained_diagnostics.saturating_add(keep);
        let mut emitted = if dropped {
            self.truncation_marker().into_bytes()
        } else {
            Vec::new()
        };
        let control = self
            .control_bytes
            .saturating_add(emitted.len())
            .saturating_add(metadata.len());
        if control > CONTROL_BYTES {
            return Err(io::Error::other(
                "rustc metadata exceeds the bounded control-output allowance",
            ));
        }
        self.control_bytes = control;
        self.tail.drain(..self.tail.len() - keep);
        emitted.extend(self.tail.drain(..));
        emitted.extend_from_slice(metadata);
        self.bytes.extend_from_slice(&emitted);
        self.retained_diagnostics += keep;
        self.tail_capacity = self.limit.saturating_sub(self.retained_diagnostics);
        self.head_limit = self.bytes.len();
        self.total = self.retained_diagnostics;
        Ok(emitted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_after_truncation_streams_before_finish_with_bounded_replay() {
        let mut capture = StreamCapture::new(1024, "stderr", true);
        let mut streamed = Vec::new();
        for bytes in capture.push(&vec![b'x'; 9000]).unwrap() {
            streamed.extend(bytes);
        }
        let metadata = b"\n{\"artifact\":\"/stage/libx.rmeta\",\"emit\":\"metadata\"}\n";
        for fragment in metadata.chunks(7) {
            for bytes in capture.push(fragment).unwrap() {
                streamed.extend(bytes);
            }
        }
        assert!(streamed
            .windows(b"libx.rmeta".len())
            .any(|bytes| bytes == b"libx.rmeta"));
        for bytes in capture.push(&vec![b'y'; 9000]).unwrap() {
            streamed.extend(bytes);
        }
        for bytes in capture.finish() {
            streamed.extend(bytes);
        }
        let retained = capture.into_bytes();
        assert_eq!(streamed, retained);
        // One metadata event and at most two small truncation markers may sit
        // outside the 1 KiB diagnostic budget, even after both 9 KiB bursts.
        assert!(retained.len() <= 1024 + metadata.len() + 256);
    }

    #[test]
    fn repeated_metadata_cannot_exhaust_unbounded_memory() {
        let mut capture = StreamCapture::new(1024, "stderr", true);
        let metadata = b"{\"artifact\":\"/stage/libx.rmeta\",\"emit\":\"metadata\"}\n";
        for _ in 0..CONTROL_BYTES / metadata.len() {
            capture.push(metadata).unwrap();
        }
        assert!(capture.push(metadata).is_err());
    }
}
