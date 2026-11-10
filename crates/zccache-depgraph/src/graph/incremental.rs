//! Queue keys and retention ages; capture current rows on the blocking writer.

use super::{ContextEntry, ContextKey, ContextRegistration, DepGraph};
use std::sync::atomic::Ordering;

impl DepGraph {
    pub(crate) fn queue_context(&self, key: ContextKey, entry: &ContextEntry) {
        self.pending_contexts
            .insert(key, Some(entry.last_accessed_unix_ms));
        self.mark_dirty();
    }

    pub(super) fn queue_registration(
        &self,
        registration: ContextRegistration,
    ) -> ContextRegistration {
        if let Some(entry) = self.contexts.get(&registration.map_key) {
            self.queue_context(registration.map_key, &entry);
        }
        registration
    }

    pub(crate) fn queue_removed_context(&self, key: ContextKey) {
        self.pending_contexts.insert(key, None);
    }

    pub(super) fn record_context_access(&self, key: ContextKey, accessed: u64) -> u64 {
        self.pending_contexts
            .entry(key)
            .and_modify(|pending| {
                if let Some(previous) = pending {
                    *previous = (*previous).max(accessed);
                }
            })
            .or_insert(Some(accessed));
        self.mark_dirty();
        accessed
    }

    /// Startup validation rejected the primary snapshot, even if quarantine failed.
    /// The owning loader authorizes rebuilding its journal from live recovered state.
    pub fn reject_legacy_snapshot(&self) {
        self.legacy_rejected.store(true, Ordering::Release);
        self.schedule_incremental_migration();
    }

    pub(crate) fn schedule_incremental_migration(&self) {
        self.migration_requested.store(true, Ordering::Release);
    }

    /// Whether context changes or a legacy import await their per-entry commit.
    #[must_use]
    pub fn has_pending_contexts(&self) -> bool {
        !self.pending_contexts.is_empty()
            || self.clear_requested.load(Ordering::Acquire)
            || self.migration_requested.load(Ordering::Acquire)
    }
}
