# zccache-fscache integration tests

- `same_stamp_edit_test.rs` — regression for #1943: an in-place edit that
  keeps size and restores mtime must not return the stale content hash.
- `warm_lookup_budget_test.rs` — `Duration` budget: a warm `lookup_since`
  stays one stat and zero hashing with the identity check in place.
