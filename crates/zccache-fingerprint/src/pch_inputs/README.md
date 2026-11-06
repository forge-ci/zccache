# pch_inputs

Strict clang PCH input validation with compiler-private timestamp views.

- `bitstream.rs` reads a clang PCH's `CONTROL_BLOCK` / `INPUT_FILES_BLOCK`
  records (spelling, size, mtime, xxh3 content hash): exactly what clang
  compares when a consumer loads the PCH. Only the head of the file is read.
- `mod.rs` decides whether a cached PCH may be served into the current
  workspace (`plan_pch_serve`).
- `overlay.rs` reads and verifies inputs again, writes independent snapshots
  in a caller-owned private directory, and sets only those snapshots' mtimes
  to clang's recorded seconds (`write_pch_overlay`). The resulting JSON VFS
  overlay maps original absolute paths and preserves their names with
  `use-external-names: false`. The caller retains the directory until the
  compiler exits. Source headers and imported PCHs are never modified.
- `tests.rs` builds real PCHs with `clang++` under strict flags
  (`-Werror=invalid-pch -verify-pch -fpch-validate-input-files-content`) and
  covers each decision branch.
- `overlay_tests.rs` checks source timestamp preservation, independent
  snapshots, content and imported-PCH size changes, conflicting records,
  and private-file collisions without requiring clang.

Decision per recorded input: missing, size or content mismatch, or a PCH whose
top-level header is a different file (another checkout) is refused; an
mtime-only mismatch uses a private snapshot with the recorded mtime, but only
for a file under the replay root and only while `ZCCACHE_PCH_MTIME_REPLAY` is
not `0`/`false`/`off`/`no`. Header snapshots must match size and xxh3 content;
imported PCH snapshots must match size and a verified content hash. Ordinary clang PCH imports have no
signature: callers must verify imported bytes against provenance recorded
when their parent PCH was produced before authorizing a snapshot.
PCH producers freeze every loaded PCH binary before compiler execution,
including binaries whose timestamps already match. Their provenance captures
those executed snapshots rather than mutable workspace files afterward.
Changes between planning and snapshotting are refused. A failure may leave
private snapshots for caller cleanup, but never changes a source mtime or
publishes a completed overlay. This preserves other build directories'
dependency tracking, including nanosecond source timestamps.
