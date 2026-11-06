# ban_std_fs_outside_adapters — sources

- `lib.rs` — late-pass visitor that fires on any path resolving into
  `std::fs` (module-qualified calls, `use std::fs::…` items, and `std::fs`
  types such as `File`).
- `allowlist.txt` — newline-separated tail-suffix matches for source files that
  pre-date the lint. Remove entries as each file moves onto the kernal-api fs
  capabilities.

See the parent directory's `README.md` for the user-facing rationale.
