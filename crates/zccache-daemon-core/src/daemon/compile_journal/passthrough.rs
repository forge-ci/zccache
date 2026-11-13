//! Preserve the compile pipeline's deliberate cache-refusal decision (#1865).

use std::cell::RefCell;
use std::future::Future;

use super::{miss_reason, record_miss_reason, JournalEntry};

kernal_api::task_local! {
    static ACTIVE_PASSTHROUGH_REASON: RefCell<Option<String>> = ACTIVE_PASSTHROUGH_REASON_TLS;
}

/// Capture the already-computed refusal reason within one request only.
pub async fn capture_passthrough_reason<F: Future>(future: F) -> (F::Output, Option<String>) {
    ACTIVE_PASSTHROUGH_REASON
        .scope(RefCell::new(None), async move {
            let output = future.await;
            let reason = ACTIVE_PASSTHROUGH_REASON.with(|slot| slot.borrow_mut().take());
            (output, reason)
        })
        .await
}

/// Record at the refusal boundary; never infer a refusal from a cache probe.
pub fn record_passthrough_reason(reason: String) {
    record_miss_reason(miss_reason::UNCACHEABLE_INPUT);
    let _ = ACTIVE_PASSTHROUGH_REASON.try_with(|slot| {
        *slot.borrow_mut() = Some(reason);
    });
}

impl JournalEntry {
    /// Successful deliberately uncached compiles are skips; failed compiles,
    /// cached results, and cacheable misses retain their original classification.
    pub fn with_passthrough_reason(mut self, reason: Option<String>) -> Self {
        if self.outcome == "miss" && self.exit_code == 0 && reason.is_some() {
            self.outcome = "passthrough";
            self.passthrough_reason = reason;
            self.miss_reason = None;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::compile_journal::{capture_miss_reason, tests::make_ctx};

    #[test]
    fn passthrough_preserves_errors_hits_and_real_misses() {
        for (outcome, exit_code) in [
            ("error", 1),
            ("error", -143),
            ("cached_error", 1),
            ("hit", 0),
            ("link_hit", 0),
        ] {
            let entry = JournalEntry::new(make_ctx(vec![]), outcome, exit_code, 0, None)
                .with_passthrough_reason(Some("parser refusal".into()));
            assert_eq!(entry.outcome, outcome);
            assert!(entry.passthrough_reason.is_none());
            assert!(entry.miss_reason.is_none());
        }
        let entry = JournalEntry::new(
            make_ctx(vec![]),
            "miss",
            0,
            0,
            Some(miss_reason::CONTEXT_NOT_FOUND),
        )
        .with_passthrough_reason(None);
        assert_eq!(entry.outcome, "miss");
        assert_eq!(
            entry.miss_reason.as_deref(),
            Some(miss_reason::CONTEXT_NOT_FOUND)
        );
    }

    #[tokio::test]
    async fn passthrough_reason_is_scoped_and_preserves_the_parser_string() {
        let (((), miss, _, _), reason) =
            capture_passthrough_reason(capture_miss_reason(Box::pin(async {
                record_passthrough_reason("non-cacheable crate type: custom".into());
            })))
            .await;
        assert_eq!(miss, Some(miss_reason::UNCACHEABLE_INPUT));
        let entry =
            JournalEntry::new(make_ctx(vec![]), "miss", 0, 0, miss).with_passthrough_reason(reason);
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["outcome"], "passthrough");
        assert_eq!(
            json["passthrough_reason"],
            "non-cacheable crate type: custom"
        );
        assert!(json.get("miss_reason").is_none());
        let (_, reason) = capture_passthrough_reason(async {}).await;
        assert!(reason.is_none());
    }

    /// #1865: classification reuses the result and reason; no I/O is required.
    #[test]
    fn perf_passthrough_classification_budget() {
        let mut entry = JournalEntry::new(make_ctx(vec![]), "miss", 0, 0, None);
        entry.passthrough_reason = Some("test harness link product not cacheable".into());
        let start = std::time::Instant::now();
        for _ in 0..100_000 {
            let reason = entry.passthrough_reason.take();
            entry.outcome = "miss";
            entry = std::hint::black_box(entry).with_passthrough_reason(reason);
            assert_eq!(entry.outcome, "passthrough");
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }
}
