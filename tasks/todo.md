# #1116 container summary handoff

- [x] Reproduce the real Linux campaign failure: root-owned 0644 summary,
  host-owned parent directory, non-root host enrichment gets PermissionDenied.
- [x] Observe RED for summary enrichment without file write permission.
- [x] Publish a JSON replacement through the owned directory; preserve
  existing metric values and failed performance status.
- [x] Split artifact helpers to keep the touched source below 1,000 lines.
- [x] Run standalone campaign infrastructure tests: 41 passed.
- [ ] Review, run the complete isolated local gate, push and merge the PR.
- [ ] Collect fresh full campaign evidence on the fixed committed runner;
  retain the pre-fix sample separately without mixing commit identities.

# Link output permissions

- [x] Reproduce a successful fresh link followed by a non-executable cache hit.
- [x] Add executable/sidecar mode coverage across delivery modes and restart.
- [x] Retain modes in the artifact index; migrate legacy snapshots and re-link old entries.
- [x] Verify a real compiler miss, hit, and restart hit through the fixed binary.
- [x] Complete crate tests, real linker integration, and workspace lint.

Validation: 1,183 daemon/artifact tests and the real Clang/LLD regression
pass. Full `./lint` passes, including Linux/Windows/macOS Dylint and rustdoc.
Real compiler miss, hit, and restart-hit verification pass.

# PR #1655 rebase + Windows/dylint fixes

- [x] Rebase `repair-3` onto main (resolve 5 conflicted files: keep both `current_rustc_out_dir` #1749 and PR's `depfile_key_root`; port moved cached_hit tests).
- [x] Add platform-gated `from_root(relative)` fixture generator to `zccache-test-support`.
- [x] Move `depfile_root.rs` inline tests to `depfile_root_tests.rs` (dylint `enforce_platform_boundary` failure: `#[cfg(unix)]` + `os::unix::fs::symlink` in a Production-classified file).
- [x] Convert all POSIX-root test fixtures (`/work/tree`, `/wt/a`, ...) to `from_root(...)` (5 Windows test failures: `root_spelling` requires `is_absolute`, false for `/...` on Windows).
- [x] Verify: targeted tests, clippy, fmt, and `uv run python -m ci.lint --dylint-only`.
- [x] Push branch, open PR, validate CI (landed on main as #1757).
- [x] Confirm Windows drive-letter, `\\` and case handling (review ask on #1655): `cfg(windows)` tests for a backslash root round trip, drive-letter-case and forward-slash spellings that bind, and an on-disk directory-case alias that binds.

## Review — PR #1655 rebase

- The rebase missed two `CachedHitMaterializeRequest` literals in `cached_hit_output_path_tests.rs` (new on main via #1749); added `depfile_key_root: None`.
- Local: `zccache-daemon-core` lib tests 1007 passed; `ci.lint --dylint-only` exit 0; clippy on touched crates (`--no-deps`) clean; `--target x86_64-pc-windows-msvc` test check compiles.
- Already on main, not part of this PR: `clippy::nonminimal_bool` at `zccache-core/src/config/env_policy.rs:156`.

# #1719 embedded flush consistency

- [x] Inventory startup loads, flush/shutdown/periodic writers, the in-memory WAL, and per-file durability guarantees.
- [x] Test that a newer artifact index with older auxiliary snapshots restarts and serves the current source contents.
- [x] Move independent snapshots before the pending depgraph load wait and verify flush step order.
- [x] Document interruption outcomes and the remaining deadline-aware flush recommendation.
- [x] Run focused tests, formatting, workspace lint, and local review.

# #1613 source-tree PCH fixture compile phase

- [x] Reproduce both original #1609 PCH fixtures independently and capture their compiler/daemon evidence.
- [x] Determine whether source-tree PCH generation and build-directory sub-header invalidation share a parser/output-plan cause. They do not: retain the real stale-hit bug as #1609.
- [x] Correct the stale source-tree fixture assumption by adding the explicit `-c` compile phase to its initial PCH generation and rebuild, retaining real PCH cache coverage.
- [x] Run focused RED→GREEN tests, affected PCH/integration coverage, format, warnings-denied checks, and review. Source-tree regression passes; remaining PCH suite is 3 passed / 0 failed with #1609 excluded; formatter, warnings-denied cache-native Clippy, and Windows target test compile pass.
- [x] Record results, commit/push/open the focused PR, and leave the branch clean.

## Review — #1613

- **Root cause:** the source-tree PCH fixture omitted `-c` on both initial PCH generation and rebuild, so clang rejected its explicit `-o` before exercising cache behavior.
- **Fix:** add the explicit compile phase to both generation commands, matching the other PCH fixtures.
- **Scope split:** #1609 remains open for the independent consuming-PCH transitive-header stale-hit defect.
- **Validation:** focused RED→GREEN test, non-stale PCH integration tests (3 passed), formatter, warnings-denied cache-native Clippy, and Windows target test compile pass.
- **Review:** medium local code review found no correctness findings and confirmed the explicit `-c` PCH generation shape is parser-supported.

# #1607 response-file journal fixture

- [x] Verify the #1532 response-file parser/journal contract and identify whether the current failure is dispatch behavior or a stale test assumption. Direct dispatch correctly receives the caller-owned `@rsp`; the local harness intercepted the fixture's `/bin/rm`, not product behavior.
- [x] Preserve the original-file deletion timing without requiring the compiler to receive the caller-owned `@rsp` token. The fixture skips identity probes and atomically moves the original response file aside during the real direct compiler invocation.
- [x] Run the focused RED→GREEN regression, daemon-core validation, formatter, warnings-denied checks, and a local review. Focused test, daemon-core suite (877 passed), formatter, warnings-denied rustdoc/Clippy, and full workspace units pass.
- [x] Record results, commit/push/open a focused PR, and leave the branch clean.

## Review — #1607

- **Root cause:** the local command guard replaces the fixture's actual `rm` child process with a denied command response. The response-file path and direct compiler dispatch are correct.
- **Fix:** move the caller response file to a sibling during the non-probe fixture invocation; assert the original path is unavailable and the moved file exists before the asynchronous journal row is read.
- **Validation:** focused RED→GREEN test, all 905 daemon-core unit tests (877 active / 28 ignored), full no-cache workspace unit suite, formatter, warnings-denied rustdoc, and affected all-target Clippy pass.
# #1608 Windows-only DLL integration fixtures

- [x] Verify that the failing DLL fixtures use Windows-only source/compiler assumptions while portable cache-native tests remain in the same module.
- [x] Gate only the three DLL fixtures on Windows; retain all portable executable and compiler-driver test coverage.
- [x] Validate the Linux ignored-test selector, formatting, test compilation, warnings-denied checks, and local review. Linux cache-native selector: 40 passed / 0 failed after excluding separately tracked #1609 PCH tests; Windows cache-native target compiles with warnings denied.
- [x] Record results, commit/push/open the focused PR, and leave the branch clean.

## Review — #1608

- **Root cause:** three `__declspec(dllexport)`/`.dll` fixtures were selected by Linux `--ignored` runs even though they require a Windows compiler environment.
- **Fix:** apply `#[cfg(windows)]` only to the three DLL fixture tests. Portable executable and compiler-driver coverage in the same module stays active on Linux.
- **Validation:** Linux cache-native integration selector 40 passed / 0 failed (excluding #1609); Windows `cache_native` test target warnings-denied check and local cache-native Clippy pass.
- **Review:** medium local code review found no correctness findings.

# #1615 watcher predicate KeyboardInterrupt notification

- [x] Reproduce the native watcher interruption with the workflow-equivalent PyO3 extension setup and trace predicate/dispatch ownership. Baseline: 11 tests passed, then pytest aborted from a leaked `KeyboardInterrupt`.
- [x] Ensure a dispatch-thread predicate `KeyboardInterrupt` requests exactly one main-thread interruption while preserving direct main-thread propagation and one stop log.
- [x] Keep the public and watcher-package Python implementations behaviorally aligned; add deterministic mocked-notification coverage.
- [x] Run watcher native tests, formatter/lint checks, Rust extension validation, and a local review; remove generated extension artifacts. CI-equivalent watcher native pytest: 14 passed; Rust watcher tests: 60 passed; PyO3 host strict Clippy passed; staged extension/bytecode artifacts removed. Public-package pytest was blocked by the repository Python-test policy.
- [x] Update #1615 evidence, commit/push/open the focused PR, and leave the branch clean.

## Review — #1615

- **Root cause:** predicate exception handling notified the main thread and re-raised; the outer dispatch handler caught the same exception and notified it again, leaking a second process-wide `KeyboardInterrupt` after the test assertion completed.
- **Fix:** the predicate path logs once and re-raises; the dispatch loop is the single owner of `interrupt_main` notification.
- **Validation:** workflow-equivalent native watcher pytest goes from 11 passed plus interrupt to 14 passed, including a mocked exact-one-notification regression; Rust watcher tests and strict PyO3 Clippy pass.
- **Review:** medium local code review found no correctness findings.

# #1597 cache-hit executable restore race

- [x] Read the artifact materialization and execution ownership contracts; map cache-hit restoration through compiler dispatch.
- [x] Reproduce the build-script `ETXTBSY` failure with a deterministic regression that exercises a cache-hit restore racing execution.
- [x] Make the minimal synchronization/ownership fix without adding a dependency; preserve hit-path concurrency where execution is not involved.
- [x] Run focused regression tests, affected crate suites, formatting, warnings-denied checks/clippy, rustdoc where applicable, and local integration validation. Focused #1597 and #1562 regressions, formatting, docs, Clippy, helper-binary prebuild, all-target workspace check, and Windows cross-check pass. Full no-cache workspace units have one unrelated reproducible #1607 failure; the integration lane has unrelated #1608 Windows-DLL fixture and #1609 PCH failures.
- [x] Review the final diff. Medium code review found no synchronization, portability, performance, or test-coverage finding.
- [x] Commit, push, open the resolving PR, then verify no temporary files or worktree changes remain.

## Review — #1597

- **Root cause:** `persist/write_cached.rs` directly materialized legacy cache-file and inline payload hits without the existing spawn/materialization exclusion. A daemon child forked during that write can inherit the descriptor, and a Cargo hard-linked build-script alias then fails `execve` with `ETXTBSY`.
- **Fix:** take the existing exclusive materialization guard around every file-backed cache-hit delivery and inline-byte destination write. No dependency, IPC, wire, or policy change.
- **RED → GREEN:** `cache_hit_materialization_waits_for_child_spawn` fails before the guard and passes after it; the existing staged #1562 executable race regression remains green.
- **Local validation:** formatter, rustdoc with warnings denied, affected all-target Clippy with warnings denied, prebuild of all test helper binaries, and workspace all-target check pass. Full no-cache workspace units identify only #1607; ignored integration identifies only #1608/#1609 platform/fixture follow-ups. Medium code review: no findings.

# Pin kernal-api from crates.io (drop `_vender/kernal-api`) — done

Goal: `feat/complete-kernal-integration` depends on a published, exact
kernal-api with no `_vender` path. The branch had been built on the closed
kernal-api#258, whose running-process re-exports kernal-api#263 forbids and
whose ~60 owned platform items main never merged.

- [x] kernal-api facades, merged on green: daemon identity/control (#328),
      process session knobs (#331), race macros + `task_local!` (#330),
      broker client (#329) -> released 0.1.16.
- [x] kernal-api ports of #258's owned items: fs (#336), ipc (#333),
      process/host + memory readers (#335), async_engine + allocator (#334).
- [x] Final gaps found by building zccache against main (#337): `Mutex` with
      Arc-retaining owned guards, `Notify` futures, `Task::detach_on_drop`,
      `RuntimeBuilder::max_blocking_threads`, `VerifiedDaemon::has_exited`,
      `SyncEnvironment::UserBaseline`, `DirectoryWalk::parallelism`,
      `ProcessOutputFault::new` -> released 0.1.17.
- [x] B0-B5 zccache migration: frame codec, identity/probe/control, process
      sessions (+ child RSS sampling), daemon launch, broker client.
- [x] Restore the two behaviors the migration lost: daemons start from the
      user's login environment (`UserBaseline`); `mtime_replay` walks on a
      dedicated pool (soldr#2760).
- [x] B6 Pin `kernal-api = { version = "=0.1.17", default-features = false }`,
      remove `_vender` and its workspace exclude, regenerate the lockfile.

## Review

Validation against the crates.io release (no path/patch override; the
lockfile resolves `kernal-api 0.1.17` from the registry, no `_vender` entry):
workspace check 0, workspace clippy `-D warnings` 0, `./test` 2734 passed /
0 failed, wire characterization 45 passed with no golden bytes changed,
`ci/check_kernal_api_baseline.py` 0. Dylint could not run on this host
(dylint-driver needs `libz.so.1`); CI runs it.

Lessons: build the consumer against the producer's real main *before* cutting
a release -- the first attempt assumed four surfaces were the whole gap and
found ~60 more only at compile time. A probe patch that makes the consumer
compile is a fast, exact gap list.

# kernal-api#13 portable Rustc policy

- [x] RED missing output-plan seam; GREEN 384 compiler tests after extraction.
- [x] Preserve native explicit-output and directory-join semantics across host facts (24-case fixture; complete 385-test compiler suite passes).
- [x] Separate lexical path syntax from compiler host and expose a native-dependency-free policy surface (only typed-path remains).
- [x] Validate 387 native tests, three no-default public API tests, native/no-default strict Clippy, no-default docs, 11 packaging tests, and Astra review.
- [ ] Execute identical parser fixtures through Core and Component guests, then review and merge.
- [ ] Complete controlled cache hit/miss and bounded process-output proof.

# kernal-api#13 resumable request encoding

- [x] RED: compare a resumable cursor with the literal existing byte protocol (missing cursor, build log 20260913T103304Z).
- [x] Share one encoding implementation between synchronous native sinks and async guest consumers (24 hash tests and strict all-target Clippy pass).
- [x] Preserve lazy argv consumption and first-error behavior; test, lint, review (Astra clean; focused test independently passed).
- [x] Isolate encoder-only guest dependencies; Wasm check and empty normal dependency graph pass. Native 24 tests, encoder-only three tests, packaging ten tests, strict Clippy and Ruff pass.
- [ ] Finish downstream daemon fingerprint validation, then open the coordinated PR.
- [ ] Follow with actual kernel hash execution and portable compiler policy; crate compilation alone is not runtime acceptance.

# kernal-api#13 explicit Rustc parser host facts

- [x] RED: inject Linux/macOS/Windows naming facts independently of the test host.
- [x] Preserve the native entry point while passing host facts into the existing parser.
- [x] Check proc-macro/bin/Dylint policy, target override, and original argv preservation.
- [ ] Run compiler tests, strict Clippy, review, and open a coordinated PR.
- [ ] Follow with portable dependency/path seams and actual kernel-hosted Wasm proof; this change alone does not make the compiler crate portable.

# kernal-api#13 shared request-fingerprint encoding

- [x] Capture literal-byte compatibility fixtures against the existing daemon encoder.
- [x] RED: expose a fallible, allocation-free byte emitter in the existing hash crate.
- [x] Make the native daemon consume the same encoder with lazy normalization.
- [x] Validate callback failure, raw depfile salts, existing normalization tests, Clippy, and review.
- [ ] Open a coordinated PR; portable host/path policy and actual kernel guest effects remain follow-up work.

Validation: original-encoder literal baseline passed; missing-emitter RED E0432;
hash 23 passed; daemon fingerprint/path-policy 29 passed; full daemon 837 passed,
28 existing ignored (139.73s). Strict selected-crate all-target Clippy with
`--no-deps` passed (92.86s), rustfmt/diff checks and Astra review clean. Broader
dependency Clippy fails on three existing `double_must_use` diagnostics in
untouched `zccache-protocol/src/wire_prost/api.rs` (173, 185, 192).

# #1555 exclusive compiler admission includes pre-hashing

- [x] Trace soldr CI's signal-terminated exclusive `soldr_cli --test` compile to zccache's post-admission Rayon pre-hash overlap.
- [x] RED: deterministically prove exclusive pre-hash work completes before the compiler phase while shared work retains overlap.
- [x] GREEN: await pre-hashing for exclusive units and pass its ready hashes into the existing store outcome without adding a global job cap.
- [ ] Run focused tests, formatter, clippy/check, and the sanctioned performance matrix.
- [ ] Review, push, merge, publish a patch release, then update Soldr's exact pin and release assets.

# soldr#3031 preserve compiler termination signals

- [x] Read the embedded-service, journal, protocol, and portability contracts.
- [x] Trace the signal-loss boundary across single, direct, multi-source, staged-multi, embedded, IPC, and journal paths.
- [x] RED: prove Unix signal termination remains distinguishable from daemon-side errors in the canonical process-status representation and compile journal.
- [x] GREEN: preserve termination metadata through every compiler response path, the embedded API, audit events, and the durable journal without changing successful/error-cache semantics.
- [x] Cover protobuf/bincode compatibility; the reserved integer encoding leaves both wire shapes unchanged, so no protocol bump is required.
- [x] Run focused tests, formatter, clippy/check, and broader compile-path validation. All signal-path tests pass; the full daemon-core suite has one unrelated filesystem-matrix mtime assertion that reproduces alone on macOS.
- [ ] Review, push, open and merge the linked zccache PR, publish a patch release, then update Soldr's exact pin and six release assets.

# #1539 embedded host compiler-admission classifier

- [x] Read the issue, service admission paths, and embedded configuration surface.
- [x] Add a RED policy/ordering contract covering host-only exclusivity and cache-hit bypass.
- [x] Add the opt-in host classifier and pass it through canonical post-hit admission.
- [x] Cover built-in predicate composition, contention/cancellation, and default compatibility.
- [x] Document the host-policy, hit-bypass, and semaphore-then-RwLock contract.
- [x] Run focused and broader validation.
- [ ] Obtain independent review, push, open the resolving PR, and monitor CI.

# #1513 replace vulnerable sevenz-rust

- [x] RED: `soldr cargo audit` reports RUSTSEC-2026-0245 through sevenz-rust 0.6.1.
- [x] Migrate the workspace dependency to sevenz-rust2 while preserving zccache's checked extraction callback.
- [x] Add an end-to-end malicious 7z traversal regression and retain the existing round-trip test.
- [x] Run focused tests, format, publish checks, and confirm the advisory/package is absent. Full feature Clippy is blocked by three pre-existing warnings in analyze.rs and engine_profile.rs; main CI is green at d8890ab2.
- [ ] Push, open and merge the linked PR, then publish a patch release for Soldr.

# #1365 centralize host-platform mechanics behind zccache-platform

Parent: https://github.com/zackees/zccache/issues/1365 — 6 phase sub-issues (#1366-#1369 exist; two more to be created for phases 5/6). Parent auto-closes when all sub-issues close. Starting branch: `refactor/platform/phase2-fs`.

- [x] Read #1365 + sub-issues #1366-#1369; baseline branch recorded.
- [x] Install Rust 1.95.0 toolchain locally.
- [ ] Phase 0: exploration inventories (MSRV pins, platform-heavy code, amalgamation/dylint/CI).
- [x] Phase 1 (#1366): PR #1370 merged.
- [x] Phase 2 (#1367): PR #1371 merged; corrective PR #1373 merged green.
- [~] Phases 3/4/5 (#1368/#1369/#1378): integrated on PR #1375; checkpoint PRs #1377 and #1379 merged into its branch.
- [~] Phase 6 (#1380): zero-baseline implementation complete locally; validation and PR gates pending.
- [ ] Finish: git status clean; local repo back on `refactor/platform/phase2-fs` rebased to final main.

Each phase: branch from main → TDD RED→GREEN → soldr fmt/clippy/check → ./test → PR → wait for GHA green (watch) → merge → rebase next phase.

## Phase 2 (#1367) plan — platform::fs

Slices per sub-issue: (1) RED facade/characterization tests, (2) identity/link/change-marker
from persist/hardlink.rs + fs_caps.rs, (3) permissions from config/paths.rs + win_acl.rs +
staged_lock.rs + rust_plan/local.rs, (4) replace/clone/positioned-io from kv.rs +
persist/{artifact_io,staged_paths,staged_store}.rs, (5) path normalization from
zccache-core/src/path.rs, (6) wire consumers (core, artifact, daemon-core) + ratchet baseline.

Characterization tests FIRST: two hardlinks == FileIdentity; copy !=; nlink increments;
volume identity stable; Windows 128-bit FileId preserved; unix ChangeMarker = None.
Baseline rows to delete: all rows for core/artifact/daemon-core fs files (per-crate counts in
commit bde74dfb). Files must stay <1000 LOC (kv.rs 1061 → split).

## Phase 3 (#1368) plan — platform::ipc

RED (2026-08-13): `soldr --no-cache cargo test -p zccache-platform` failed with eight
`cannot find ipc in platform_imp` errors after the neutral contract and characterization tests
were added before any concrete backend. The same command is GREEN with 21 passing tests after
adding the Linux, macOS, and Windows concrete transports. Final focused GREEN:
26 platform tests, 52 IPC tests, and the download-protocol endpoint test pass on Windows;
platform + IPC all-target checks pass for installed macOS and Linux cross targets. Coverage
includes simultaneous 1 MiB duplex traffic, endpoint retirement/rebind, ordinary-file
preservation on Unix, Unix 0700/0600 permissions, and the live Windows pipe DACL.

Facade: endpoint/stream/listener/connect/peer. Move zccache-ipc/src/transport/{mod,unix,windows,
pipe_security}.rs native mechanics; keep framing/protocol/broker/timeouts in zccache-ipc.
Endpoint strings byte-for-byte stable; owner-only socket/pipe security preserved; no extra
roundtrip; download_client + daemon_mgmt consume neutral APIs. Baseline rows: all IPC +
download-protocol rows deleted (390 -> 336). Workspace check and clippy with `-D warnings`
pass; `./test` passes. `./test --integration` reaches an unrelated existing contract mismatch:
`stop_kills_locked_process_when_ipc_is_unreachable` expects a fabricated PowerShell PID to be
killed, while the #1161/#132 identity-bound stop intentionally refuses non-zccache processes.

## Phase 4 (#1369) plan — platform::process

RED (2026-08-13): `soldr --no-cache cargo test -p zccache-platform` fails with
13 missing process-facade APIs after adding the capability leaves and owned-child
characterization tests before any concrete backend.

Facade: command/spawn/priority/inspect/terminate/stdio/jobserver/exit. Move daemon/{process,
child_watchdog,jobserver,trampoline}.rs native code + core/crash.rs signal labels +
ipc PID helpers + cli-core deploy.rs spawn bits + bin/zccache.rs stack wrapper. Keep
CompilePriority/watchdog budgets/jobserver accounting in daemon. Coordinate #1360 (owner-death
behavior preserved). Files <1000 LOC (process.rs 1365, child_watchdog.rs 1202 → split).

GREEN checkpoint (2026-08-13): inspect/terminate callers, watchdog CPU accounting,
priority application, Windows kill-on-close Job Object ownership, and the POSIX jobserver
primitive now use the neutral process facade. Product policy and diagnostics remain in their
callers. Platform tests pass 32/32, focused daemon jobserver tests pass 2/2, and targeted
production clippy passes with warnings denied. The exact-occurrence baseline fell from 336 to
127 after also moving stdio redirection, crash-context interpretation, command window setup,
CLI stack selection, and pre-spawn owner-death selection; its cache version is 0.1.7. Docker
Desktop's Linux engine pipe was unavailable, so the
Linux Dylint runtime gate remains required before publication; local baseline wiring validation
passes.

The two touched oversized production files were split without moving product policy:
`process.rs` is 721 lines and `child_watchdog.rs` is 696 lines; their existing test modules
now live in purpose-named sibling `_tests.rs` files.

## Phase 5 (#1378) plan — platform::executable + platform::host

executable: deploy.rs suffixes/PATH/PATHEXT/image lookup/unlock_exe. host: native_cpu.rs,
defender.rs elevation + Defender primitives, path/env OS facts, home/runtime dirs, resource probes.

RED (2026-08-13): focused platform compilation failed with nine missing concrete
`executable`/`host` backend references after adding neutral characterization tests first.
GREEN checkpoint: all three private host trees now implement executable naming/search/stem,
shared-library candidates, running-image replacement, and host OS/arch/home/runtime/user/
elevation/resource facts. Windows Defender command mechanics moved into the concrete Windows
host tree while core retains product errors and UX.
Platform/core/depgraph tests and CLI-core all-feature/all-target checks pass on Windows. The
exact-occurrence baseline fell from 127 to 97 after migrating Defender, native CPU inputs,
libclang discovery, and executable replacement.

## Phase 6 (#1380) plan — zero baseline

RED: the final audit found 97 grandfathered production occurrences across artifact, compiler,
core, daemon, depgraph, and IPC.

GREEN implementation: all 97 rows were migrated, `baseline.txt` and its runtime ratchet were
deleted, and the lint now rejects every production occurrence directly. Host cfg/native paths
scan clean outside the selector/concrete trees. Production `libc`/`windows-sys` dependencies are
confined to zccache-platform; the only other libc declaration is Unix integration-test-only.

Pending: Linux Dylint, cross-target/target-semantics checks, publish self-containment, full tests,
clippy, pre-push review, and green PR CI before merge and parent closure.

# soldr#2188 short compiler staging root

- [x] Read the issue, staged-output architecture, portability contract, and performance gate.
- [x] RED: prove an embedded host can place private compiler staging outside a deep cache root.
- [x] GREEN: add an explicit embedded staging root while preserving cache-local staging by default.
- [x] Document the ownership and cleanup contract.
- [ ] Run focused tests, formatting, clippy, and the sanctioned performance matrix.
- [ ] Review, push, merge, and validate the upstream change before updating Soldr.

# #1215 staged materialization lock contention

Issue: https://github.com/zackees/zccache/issues/1215 — `perf(persist): bound staged materialization lock contention before GC hardening`

- [x] Read the issue, PERF.md, prior #1215 PRs, staged-store ownership code, and retained evidence.
- [x] RED: add a deterministic regression/performance test proving concurrent staged deliveries share one process-local OS read lease while preserving maintenance exclusion.
- [x] GREEN: retain one shared staged-store file lock per active daemon/root, releasing it only after the final materialization lease.
- [ ] Verify the complete staged delivery matrix, formatter, clippy/check, integration tests, and the sanctioned repeat-5 Docker matrix (or document a concrete infrastructure blocker).
- [ ] Self-review the final diff for correctness, lock lifetime, cross-process GC behavior, and scope.
- [ ] Commit, push, open and merge the #1215 PR; synchronize local `main`, delete this branch, and verify a clean checkout.

## Review

- Focused RED signal: `concurrent_staged_materialization_leases_share_the_store_lock` failed with eight OS shared-lock acquisitions before the state-aware lease implementation, then passed after it.
- Production behavior: all five daemon hit delivery entry points now acquire a `SharedState`-owned staged lease; direct test helpers retain an independent acquisition path only under `cfg(test)`.
- Pending full validation and review.

# soldr#2031 running-process boundary

- [x] Inventory raw daemon process creation and existing Dylint exemptions.
- [x] Route sync and Tokio daemon children through running-process.
- [x] Replace file allowlists with production/test scope detection and UI coverage.
- [x] Make the workspace Dylint job a pull-request gate.
- [x] Pin the running-process revision that provides the required APIs.
- [x] Run focused Dylint, daemon checks, fmt, and clippy.
- [ ] Open, drive, merge, and validate the coordinated upstream PR.

# soldr#1783 safe nested Dylint caching

- [x] RED: model only the explicit nested Dylint compiler shape as Rust.
- [x] Hash driver, inner compiler, loaded lint-library contents, and output-affecting environment.
- [x] Fail open with a diagnostic for missing, malformed, or unhashable lint-library state.
- [x] Cover warm hits, invalidations, diagnostics, output/exit status, and ordinary Rust regressions.
- [x] Remove process-global dated-nightly overrides and guard against their return.
- [ ] Exercise the supported Soldr front door in upstream CI.
- [ ] After Soldr lands its shim, follow up upstream to run Dylint CI and the real benchmark through `soldr cargo dylint`.
- [x] Document Cargo incremental reuse versus cross-target/worktree cache reuse.
- [ ] Run focused tests, Linux Docker integration, lint/docs, and sanctioned performance evidence.
- [ ] Open, drive, merge, and validate the upstream PR before updating Soldr.

# Soldr embedded durability Lavra follow-up

- [x] Split oversized embedded/staged-store source files without changing public API.
- [x] Correct embedded cancellation and report-shape documentation.
- [x] Bound the index-writer lost-wakeup regression test.
- [x] Repair Dylint environment tests and cover alias/retry recovery.
- [x] Complete staged persistence module and marker documentation.
- [x] Track depfile role independently from suffix, rewrite bytes atomically, and propagate failures.
- [x] Cover arbitrary-extension and non-UTF-8 depfiles plus rewrite failures.
- [x] Run focused/full Rust, Python, docs, lint, and cross-target verification.

# #1149 secret-safe compile journal environment

Issue: https://github.com/zackees/zccache/issues/1149

- [x] Define one deny-first diagnostic allowlist for embedded and IPC journals.
- [x] Enforce filtering in context capture, entry construction, and serialization.
- [x] Add a versioned cross-repo fixture and representative secret/value tests.
- [x] Document the security and replay compatibility contract.
- [x] Run focused/full tests, fmt, clippy, rustdoc, and review.
- [ ] Open the PR, pass CI, merge, and validate main.

# #1148 bounded embedded/standalone disk retention

Issue: https://github.com/zackees/zccache/issues/1148

- [x] Confirm standalone/embedded lifecycle parity gap and exact-root ownership.
- [x] Add shared policy/scanner seams with RED tests for budgets, ages, pressure, hardlinks, and sibling sentinels.
- [x] Wire the shared pass into standalone and embedded startup/periodic lifecycles.
- [x] Expose host-callable embedded maintenance and structured reports.
- [x] Persist/catch up the daily age-maintenance deadline inside the configured root.
- [x] Update architecture/runtime documentation.
- [x] Run focused/full tests, fmt, clippy, Windows compile, and review.
- [x] Open the PR, pass CI, merge, and validate main.

# #1063 thin-v3 ownership-aware durable export

Parent: zackees/soldr#1609 · Policy: `thin-v3-lifetime-partition-v1`

- [ ] Add cache-schema v3 ownership policy/mode and per-artifact owner metadata.
- [ ] Make identity/cache keys and bundle compatibility reject cross-mode payloads.
- [ ] Filter cook-owned outputs from `cook-partitioned-v1` durable exports; fail closed on unknown ownership.
- [ ] Expose verified owner-qualified materialization and machine-readable diagnostics/metrics.
- [ ] Add adversarial save/restore coverage, then validate the public CLI and embedded soldr consumer.

# #968 wedge/timeout burn-down — near complete

Meta: https://github.com/zackees/zccache/issues/968

# #1136 PathBuf Dylint backlog

Issue: https://github.com/zackees/zccache/issues/1136

- [x] Classify and migrate persistent filesystem identities to `NormalizedPath`.
- [x] Preserve bundle-relative paths without treating them as filesystem identities.
- [x] Add regression coverage for normalized identity and relative bundle behavior.
- [x] Run targeted daemon/test-support checks.
- [ ] Validate the Linux Dylint CI job after merge.
- [ ] Ship, merge, and validate the resolving PR; close #1136.

## Merged to main ✅
- #967 (PR #969) client-disconnect cancellation
- #962 (PR #970) orphan-pipe post-exit watchdog (Mode A)
- #971 (PR #976) in_flight_exec lost-wakeup + bounded waiter + exec-spawn watchdog
- #972 (PR #978) `-vV` identity probe timeout
- #973 (PR #977) embedded flush disk-save bounds
- #891 (PR #980) CPU/output progress watchdog (Mode B) — all-platform CPU sampling
- #890 (PR #981) async/process bridge design doc (runtime.md)

## In CI (merge when green) 🔁
- #974 (PR #979) watcher consumer wakes on shutdown
- #893 (PR #983) child pid in watchdog diagnostics
- #892 (PR #984) pipe-saturation / concurrent-drain regression test

## Remaining 📋
- #894 concurrency-not-reduced test (child_watchdog tests) — DO AFTER #892 merges
  (same tests-module region → conflict otherwise). N concurrent watchdog waits on
  ~1s sleepers → assert total < serial (not serialized).
- Then close #889 (all children #890/#891/#892/#893/#894 done) + close #968.

## Extra issues filed (user requests)
- #975 internal multi-crate split for parallel compiles (single published crate)
- soldr#1465 soldr build wedge (embedded zccache cache) — silent death; ZCCACHE_DISABLE=1 recovery

## Key techniques learned (memory)
- ZCCACHE_DISABLE=1 to bypass wedge-prone build cache locally
- soldr cargo check --target <triple> to validate cfg(linux)/cfg(macos) arms locally
- Always `cargo fmt --all` (auto-fix) + real exit check (no pipe mask) before commit
- Kill orphan cargo.exe holding target/debug/.cargo-lock if builds "block"

## Design rules honored
- Progress/CPU-based watchdogs, never dumb wall-clock (links run minutes)
- Every timeout/watchdog fire: loud warn! + durable lifecycle event (forensics)
- Constants at top of file

# #975 internal crate split

Source: https://github.com/zackees/zccache/issues/975#issuecomment-4920394110

Contract:
- Split the current `crates/zccache` monocrate into internal workspace crates so git-rev/vendored source consumers compile subsystems in parallel.
- Keep `zccache` as the public facade preserving existing public module paths, feature names, and bin targets.
- Keep crates.io publication as one public crate named `zccache`; later wave must add publish-time amalgamation plus a CI guard that no internal crate is accidentally published.

Current wave:
- Wave 1 foundation carve: `core`, `hash`, `audit`, and `gha`.
- Subagents may edit disjoint module/crate files only and must not run linting, building, testing, formatting, or any executable command.
- Main agent owns shared workspace manifests, facade wiring, and all verification/fixups with bounded `soldr` commands.

Wave 1 status:
- Added unpublished internal crates `zccache-core`, `zccache-hash`, `zccache-audit`, and `zccache-gha`.
- `zccache` facade now re-exports those crates as `zccache::core`, `zccache::hash`, `zccache::audit`, and feature-gated `zccache::gha`.
- Verified on 2026-07-09: focused `cargo check`, facade all-target/all-feature check, workspace all-target/all-feature check, `cargo fmt --all` via `soldr --no-cache`, workspace clippy all-target/all-feature, focused new-crate tests, and `./test`.

Next wave:
- Carve `symbols`, `download`, `fscache`, `compiler`, `artifact`, and `compile_trace` into new internal crates.
- Repurpose the existing `crates/zccache-fingerprint` Python-extension crate into the internal fingerprint engine crate with Python bindings gated behind its existing `python` feature.

Wave 2 status:
- Added unpublished internal crates `zccache-artifact`, `zccache-compile-trace`, `zccache-compiler`, `zccache-download`, `zccache-fscache`, and `zccache-symbols`.
- Repurposed existing `zccache-fingerprint` into the internal fingerprint engine crate while preserving the `python` extension feature.
- `zccache` facade re-exports these crates on the old public paths and forwards `download`, `symbols`, `cli`, and `gha` features as needed.
- Verified on 2026-07-09: Wave 2 crate check, facade all-target/all-feature check, workspace all-target/all-feature check, `cargo fmt --all` via `soldr --no-cache`, workspace clippy all-target/all-feature, focused internal-crate tests, and `./test`.

Next wave:
- Carve `depgraph` and `download_protocol` into internal crates.
- Repurpose existing `crates/zccache-watcher` Python-extension crate into the internal watcher engine crate with Python bindings gated behind its existing `python` feature.

Wave 3 status:
- Added unpublished internal crates `zccache-depgraph` and `zccache-download-protocol`.
- Repurposed existing `zccache-watcher` into the internal watcher engine crate while preserving the `python` extension feature.
- `zccache` facade re-exports `depgraph`, feature-gated `download_protocol`, and `watcher` on the old public paths.
- Verified on 2026-07-09: Wave 3 crate check, facade all-target/all-feature check, workspace all-target/all-feature check, `cargo fmt --all` via `soldr --no-cache`, workspace clippy all-target/all-feature, focused Wave 3 tests, and `./test`.

Next wave:
- Carve `protocol` into `zccache-protocol`.
- Then carve `ipc` into `zccache-ipc` once `protocol` is available.

Wave 4 status:
- Added unpublished internal crate `zccache-protocol`.
- Moved the daemon protocol module and protobuf build into `zccache-protocol`.
- `zccache` facade re-exports `zccache_protocol` on the old `zccache::protocol` path.
- Verified on 2026-07-09: protocol crate all-target/all-feature check, facade all-target/all-feature check, workspace all-target/all-feature check, `cargo fmt --all` via `soldr --no-cache`, and focused protocol tests.

Next wave:
- Carve `ipc` into `zccache-ipc` now that `zccache-protocol` owns protocol types.

Wave 5 status:
- Added unpublished internal crate `zccache-ipc`.
- Moved the IPC transport, broker, manifest, and process helpers into `zccache-ipc`.
- `zccache` facade re-exports `zccache_ipc` on the old `zccache::ipc` path.
- Verified on 2026-07-09: IPC crate all-target/all-feature check, facade all-target/all-feature check, workspace all-target/all-feature check, `cargo fmt --all` via `soldr --no-cache`, workspace clippy all-target/all-feature, focused IPC tests, and `./test`.

Next wave:
- Add publish-time single-crate amalgamation and CI guards so only the public `zccache` crate can be published.
- Do release/soldr validation and hardening after the split PR lands.

Wave 6 status:
- Added release-time `zccache` crate amalgamation for crates.io packaging while keeping the checked-in workspace split for git/path consumers.
- Marked internal and PyPI extension crates `publish = false`; crates.io publish order is now only the public `zccache` crate.
- Release publish now performs a real `cargo package` verification on the transformed crate before upload.
- Verified on 2026-07-09: focused release Python tests, full `ci/tests`, `bash ./test`, workspace clippy all-target/all-feature, and transformed `zccache` package verification with `RUSTFLAGS=-D warnings`.
# #1131 remaining cache-isolation bugs

- [x] #912: merge and validate output-directory side-effect isolation (#1132).
- [x] #1028: keep distinct mutable dependency-graph variants per worktree
  while retaining root-normalized artifact identity.
- [x] Add A/B/A, concurrent registration, bounded-variant, and snapshot
  round-trip coverage; validate the Windows worktree integration without
  `RUST_MIN_STACK`.

# #1039 capability-driven COW materialization

Issue: https://github.com/zackees/zccache/issues/1039

- [ ] Capture RED characterization for poisoning, capability selection, registry, reflink independence, readonly cleanup, and matrix reporting.
- [ ] Add per-volume-pair capability probing with cached verdicts and kill switches.
- [ ] Add 128-bit-safe file identity and hardlink materialization registry/ceiling fallback.
- [ ] Add reflink-first materialization with mtime preservation and hardlink/copy fallback.
- [ ] Enforce readonly cache blobs, mediated detach, and verify/heal behavior.
- [ ] Add parameterized filesystem fixtures with loud matrix summaries.
- [ ] Add perf regression gate and user/architecture/feature-matrix docs.
- [ ] Validate on Windows 10 and Linux Docker, then run repo lint/test/review gates.
- [ ] Push one PR with RED evidence, wait for GHA/review, fix, squash merge, verify issue closure.

# #1056 immutable staged-output burn-down

Parent: https://github.com/zackees/zccache/issues/1056
Current child: https://github.com/zackees/zccache/issues/1071

- [ ] Replace lossy staged planner `Option` results with enabled/unsupported/error outcomes.
- [ ] Attribute every compile/archive/link/exact-exec rejection to one bounded stable reason.
- [ ] Add deterministic publication, salvage, and materialization fault hooks and adversarial tests.
- [ ] Validate #1071 on Windows and Linux Docker, merge its PR(s), then close the child.
- [ ] Re-audit every remaining parent producer, filesystem, mutable-output, and perf exit gate.
- [ ] Implement and merge all remaining parent slices before closing #1056.

# Fix-forward after #1049 and soldr embedded-zccache integration

- [x] Capture RED for the obsolete `soldr update-zccache` perf bootstrap.
- [x] Build soldr with the current zccache commit checked out in its vendored submodule.
- [x] Key the staged soldr binary by both the soldr and zccache commits.
- [x] Remove the obsolete runtime pin step.
- [x] Format the #1049 Rust changes and update the performance documentation.
- [x] Run focused tests, formatting, clippy, and review gates.
- [ ] Merge upstream, bump soldr's submodule, validate, and merge downstream.
# #1117 soldr-embedded mixed-language baseline

Issue: https://github.com/zackees/zccache/issues/1117

- [x] Add RED contracts for four languages, five lifecycle phases, provenance,
  strict infrastructure validity, wrapped native compilers, and artifact fidelity.
- [x] Add a compiler-complete embedded runner derived from the pinned #1116 image.
- [x] Add a deterministic Cargo fixture covering Rust, C, C++, and Emscripten outputs.
- [x] Add `perf_local.py --embedded-matrix` with repeat/resume campaign evidence.
- [x] Retain wall/CPU/TTFB/output, RSS, cache/artifact, phase, command, and identity data.
- [x] Run focused tests, Linux Docker diagnostics, and five valid samples per cell.
- [ ] Publish floor dossiers, merge the PR, and close #1117.

# DashMap read-snapshot investigation

- [x] Classify DashMaps by lookup/write semantics and select viable candidates.
- [x] Inspect existing performance evidence for lock or lookup overhead.
- [x] Bound the possible win from retained warm-path phase profiles.
- [x] Evaluate merge cost, stale-read correctness, memory, and operational complexity.
- [x] Report whether the design is worth implementing and where.
- [x] Shorten artifact lookup guards before considering a two-map redesign.
- [x] Share lazy payload and access state across owned cache-entry clones.

# Source-cache session immutability and GC investigation

- [x] Define source immutability boundaries for one compile and a build session.
- [x] Trace watcher, lookup, hashing, and metadata-cache mutations.
- [x] Audit periodic memory GC and explicit clear during active compiles.
- [x] Check regression tests and history for concurrent-change contracts.
- [x] Report whether an immutable per-session snapshot is safe.

# Traffic-aware source metadata GC investigation

- [x] Find existing compile/activity and memory-pressure signals.
- [x] Determine how to split candidate discovery from conditional deletion.
- [x] Design idle deferral with bounded starvation and memory-pressure escalation.
- [x] Identify candidate freshness, hot-entry, and journal cleanup requirements.
- [x] Report the recommended implementation shape.

# Traffic-aware source metadata GC implementation

- [x] Track complete compile-cache requests with an RAII activity guard.
- [x] Split metadata eviction into read-only candidate collection and conditional deletion.
- [x] Preserve entries whose verification timestamp changes between the two passes.
- [x] Defer blocking deletion for an idle grace period, then use nonblocking batches.
- [x] Force a blocking pass after bounded persistent traffic.
- [x] Distinguish lock-busy candidates from timestamp-refreshed candidates so
  forced completion retries only the former.
- [x] Settle deferred journal cleanup when traffic becomes idle or escalation fires.
- [x] Keep disk-maintenance scans read-only and take the publication writer
  only for the revalidated destructive commit.
- [x] Count compile, link, generic-exec, and exec-probe metadata consumers.
- [x] Cover blocking and nonblocking timestamp races with focused tests.
- [x] Cover gentle-to-forced handoff, held-shard retry, and maintenance/Clear
  artifact-lease ordering with deterministic tests.

# Dylint sibling env-dep identity follow-up

- [x] Reproduce the real-driver sibling miss by making rustc record
  `DYLINT_LIBS` as an env dependency while library paths differ by worktree.
- [x] Fold path-valued `DYLINT_LIBS` env deps through the existing synthetic
  Dylint content identity for lookup, freshness, and publication.
- [x] Validate the focused integration, depgraph suite, formatting, clippy,
  and review gate in Linux Docker.
- [ ] Merge the upstream PR, bump soldr's vendored commit, rerun the hosted
  real Dylint/watchdog acceptance, and close the meta issue.

# #1400 stable CLI-owned cache paths

- [x] Reproduce the current-main cargo-registry path mismatch from Actions on all three OSes.
- [x] File #1400 and add it to the #1205 burn-down tracker.
- [x] Add RED tests proving daemon namespaces do not move CLI-owned shared cache paths.
- [x] Keep daemon-owned state namespaced while restoring cargo-registry and symbols to the stable cache root.
- [x] Run focused tests, formatter, checks, clippy, and the locally available workflow contract.
- [x] Review, commit, push, merge the PR, close #1400, and verify current main.

## Review

- Current-main evidence: `cargo-registry save` writes below
  `daemon-state/<dev-binary-hash>/cargo-registry`, while `cache-root` reports
  the stable effective root expected by the public CLI and action contract.
- RED: the real CLI test failed with the archive under
  `daemon-state/dev-binary-hash/cargo-registry`; the production path helpers
  now keep both Cargo-registry and symbol payloads under the stable root.
- GREEN: the focused core path tests, the real cargo-registry CLI regression,
  the symbols caller contract, `zccache-core`'s full suite, the full
  `cli_cache_root` integration test, formatting, `zccache-core` clippy, and
  the feature-complete `zccache` check pass. The local workflow harness reaches
  its Docker preflight, but Docker Desktop is unavailable; the hosted
  three-OS action job remains the authoritative workflow validation.
- Review follow-up: an isolated symbols caller subprocess sets a non-empty
  daemon namespace and requires the shared cache directory to be exactly
  `<default_cache_dir>/symbols`, rejecting a stale
  `daemon-state/<namespace>` intermediate path.
- PR #1401 merged as `d11f4bde`; all three hosted cargo-registry jobs passed
  and #1400 closed automatically.

# #1404-#1409 / #1412 current-main CI stability

- [x] Reproduce and fix the namespace-mismatched daemon lockfile budget test (#1404).
- [x] Characterize the detached exec/Clear handoff timeout and keep its lock-order proof deterministic (#1405).
- [x] Make the warm-hit unit performance sentinel robust to hosted Windows runner noise (#1406).
- [x] Run build-harness journal cleanup after failed tests so the strict audit owns only runtime logs (#1407).
- [x] Allow explicit scheduler tolerance around pending-write timeout wakeups (#1408).
- [x] Remove obsolete legacy-toolchain allowlist entries while keeping the checker exact (#1409).
- [x] Make wrapper failure-boundary lifecycle fixtures namespace-aware (#1412).
- [x] Add RED contract coverage for each deterministic regression.
- [x] Run focused tests, formatting, checks, clippy, and the repository review gate.
- [x] Push, merge, close #1404-#1409 and #1412, and verify current-main CI.

## Review

- #1404 hosted RED: the development daemon wrote its namespaced lockfile within
  the nine-second cap, while the parent test polled the legacy unnamespaced path.
- #1405 hosted RED: the queued Clear future timed out after the publisher
  completed its single-guard handoff under the Linux x86 daemon-core suite.
- #1406 hosted RED: one Windows x86 sample took 2.1811039s against a 2s absolute
  budget; the dedicated COW performance guard passed in the same run.
- #1407 hosted RED: a preceding workspace failure skipped the success-only
  cleanup, then the unconditional audit reported 188 embedded v1.12.15
  build-harness journal records as integration-runtime violations.
- #1408 local full-suite RED: a 5ms pending-write timeout resumed at
  100.2205ms and failed an upper assertion of 100ms by 220.5us.
- #1409 local RED: the exact legacy-toolchain checker listed three files that
  no longer contain the legacy marker and failed on its stale allowlist.
- #1412 hosted RED: the wrapper contract tests read the legacy lifecycle log
  while development children wrote namespaced events; the refusal exit code
  was correct, but the test saw no refusal or spawn events.
- GREEN: all focused tests pass; the full daemon-core suite passes with 754
  active tests and 25 ignored; both lockfile-budget tests pass; the workflow
  and toolchain Python contracts pass; all three ignored wrapper-boundary
  tests pass; feature-complete check, clippy with warnings denied, formatting,
  and diff whitespace checks pass.
- Hosted GREEN: PR #1410 passed the full matrix, including MSRV, Dylint,
  wrapper failure boundaries, full workspace integration, strict artifact
  audit, and all platform jobs; it merged as `502827e3` and closed all seven
  linked issues.

# #1411 resumable standalone performance fixture

Issue: https://github.com/zackees/zccache/issues/1411

- [x] Add RED coverage proving resume never rebuilds or replaces the recorded fixture.
- [x] Keep every retained executable campaign-local and verify it read-only.
- [x] Preserve the normal build-and-record path for fresh campaigns.
- [x] Run focused tests, lint/format checks, and the review gate.
- [ ] Push, merge, close #1411, and resume the #1116 campaign.

## Review

- RED: resume called `_build_benchmark` before comparing the recorded fixture
  digest, replacing the shared artifact before it could detect drift.
- GREEN: 35 focused standalone/embedded tests pass; shell syntax, Python
  syntax, E/F lint, and diff checks are clean.
- Docker: a fresh interrupted campaign built and verified both local fixture
  executables; `--resume` ran verify without a build and preserved both hashes
  exactly before correctly refusing timing on unrelated active host compilers.
- Review follow-ups: retain both runtime executables per campaign, hash both,
  reject missing or changed artifacts before sampling, and mount both the
  fixture and its parent results alias read-only during verification.
- clud-review: clean (one reviewer).

# #864 soldr-driven release cross-compilation

Issue: https://github.com/zackees/zccache/issues/864

- [x] Inventory release/build matrices, composite-action artifact contracts,
  and the soldr reference workflow.
- [x] Add RED workflow contracts for a bare-cargo bootstrap and the first
  Ubuntu cross target family.
- [x] Stage bootstrap zccache and drive the selected cross jobs through soldr
  zig/xwin without changing artifact shapes.
- [x] Add or verify release dry-run behavior and publish suppression.
- [x] Run workflow contracts, focused build validation, formatting, and the
  review gate.
- [ ] Push, run the hosted dry-run, merge, and close #864.

## Review

- RED: focused workflow contracts rejected the pre-migration native-runner
  matrix because it had no wrapper-free bootstrap artifact and invoked
  platform toolchains directly instead of soldr's zig/xwin drivers.
- GREEN: 36 release-manifest/toolchain contracts pass (one environment-gated
  test skipped); all edited YAML parses, and Ruff E/F and whitespace checks pass.
- Review follow-up: recheck PATH after installing LLVM, accept symlinked
  versioned `llvm-dsymutil` executables, and verify the selected tool before
  cross compilation starts.
- Hosted RED: the second dry run installed Ubuntu's LLVM meta-package, then
  missed its versioned `llvm-dsymutil` symlink and exited before compilation.
- clud-review: clean after both follow-ups (one reviewer).
- Hosted RED: all completed Linux cross lanes terminated multiple unrelated
  rustc children together with exit 255 under four-way release parallelism;
  cross lanes are now bounded to two jobs while native builds stay unchanged.
- clud-review: clean after the initial hosted toolchain follow-ups (one reviewer).
- Final clud-review: clean after bounding both binary and Python-extension
  cross-build paths (one reviewer).
- Hosted RED: serializing Zig builds fixed every signaled compiler child and
  both macOS targets completed; all four Linux targets then exposed missing
  packed `.dwp` sidecars after cache-restored link outputs, while both xwin
  lanes reproduced exit 255 at two jobs.
- Final follow-up: serialize xwin too, and keep the normal cache-first Linux
  build while rebuilding only the shipped umbrella package without the wrapper
  when its `.dwp` outputs were not restored.
- Final clud-review: clean after restricting the repair to Zig Linux targets
  and using soldr's sanctioned no-cache fallback (one reviewer).
- Hosted RED: the next dry run cleared every Linux and macOS target, but the
  ARM64 xwin build proved cargo-xwin replaces `CFLAGS_aarch64_pc_windows_msvc`
  while injecting SDK flags, so clang-cl never received `-TP`.
- Follow-up: pass `-TP` through general `CFLAGS`, which cc-rs composes with
  cargo-xwin's target flags in this single-target ARM64 job.

# #1414 isolate session-reaping lifecycle events

- [x] Add a RED regression proving a state-owned reap event follows the daemon's explicit cache root, not the process-global environment.
- [x] Route the event through the explicit-root lifecycle writer and preserve its payload/cardinality contract.
- [x] Run the focused regression repeatedly, the daemon-core suite, formatting, and clippy/checks.
- [x] Run the repository review gate.
- [x] Push, merge, close #1414, and verify the Linux x86 gate.

## Review

- RED: with the old process-global writer, the isolated daemon log retained
  zero rows after a one-session reap (`left: 0`, `right: 1`).
- GREEN: the focused test passes against the daemon-owned root; the full
  daemon-core suite passes (754 active, 25 ignored), as do formatting,
  warnings-denied clippy for every daemon-core target, and whitespace checks.
- Post-merge GREEN: the focused regression passes against current main.
- clud-review: clean (one reviewer).
- Hosted GREEN: PR #1416 merged as `e39a9852` and closed #1414 after
  the Linux x86 check and focused matrix gates passed.

# #1418 cache packed Linux debug sidecars

Issue: https://github.com/zackees/zccache/issues/1418

- [x] Add RED coverage for staged expected outputs and legacy collected files.
- [x] Model `<primary>.dwp` for explicit Linux targets without affecting other targets.
- [ ] Prove miss/hit persistence and hosted cache-first release behavior.
- [x] Run focused tests, formatting, Clippy/checks, and the review gate.
- [ ] Push, merge, close #1418, and remove the #864 repair once hosted evidence is green.

## Review

- Hosted RED: release dry run 32355952002 restored both shipped Linux binaries
  without their packed debug sidecars and entered the wrapper-free repair.
- Local GREEN: three target/output-set regressions pass; the Linux-only real
  daemon miss/delete/hit test builds on Windows and will execute on hosted
  Linux. All-target Clippy passes with warnings denied.
- Review follow-up: packed sidecars now require effective link emission and
  enabled debug info, and repeated `split-debuginfo` values use command order.
- clud-review: clean after two finding/fix passes (one reviewer).

# #1419 preserve ordered codegen option semantics

Issue: https://github.com/zackees/zccache/issues/1419

- [x] Add RED parser and context-key coverage for reversed repeated options.
- [x] Preserve ordered codegen/linker arguments and normalize `-g`/`-O` aliases.
- [x] Bump the Rust context-key domain to prevent reuse of pre-fix artifacts.
- [x] Run focused tests, formatting, Clippy/checks, and the review gate.
- [ ] Push, merge, and close #1419.

## Review

- RED: reversed last-one-wins values sorted to the same parsed representation;
  the Dylint lane repeated that sort, and linker arguments also lost order.
- RED: `-g` and `-O` stayed in separately sorted unknown flags, so their order
  relative to explicit `-C` values was invisible to both keys and DWP modeling.
- GREEN: ordered parser, effective-value, alias-precedence, linker-order,
  Dylint-key, and context-key regressions pass; the key domain is now v3.
- GREEN: all 448 depgraph tests, six focused DWP tests, the ignored daemon
  integration build, formatting, diff checks, and warnings-denied Clippy pass.
- clud-review: clean after ordered-key follow-up fixes (one reviewer).

# #1361 separate Dylint artifact and verdict identity

Issue: https://github.com/zackees/zccache/issues/1361

- [x] Add RED coverage proving plain artifacts cannot satisfy a Dylint verdict.
- [x] Remove the Dylint input hash from artifact and metadata-compat identity.
- [x] Add a Dylint-keyed verdict layer that gates diagnostics and exit status.
- [ ] Prove build/Dylint artifact sharing without skipped lint execution.
- [x] Run focused tests, formatting, Clippy/checks, and the review gate.
- [ ] Push, merge, and close #1361.

## Review

- RED: the Dylint input hash changed the rustc artifact context key, preventing
  plain and linted invocations from sharing byte-identical outputs.
- GREEN: rustc artifact identity excludes the Dylint hash; an embedded verdict
  map keys stdout, stderr, and exit status by plain/Dylint identity and gates
  materialization before any output is replayed.
- Legacy index snapshots migrate with an empty verdict map, while malformed
  current snapshots cannot fall back to the positional legacy decoder.
- Missing verdicts are soft misses that preserve the shared artifact and its
  depgraph entry; error verdict publication preserves existing output metadata
  and follows the same ordered index-writer WAL as success publication.
- Review follow-up: cached error verdicts now return before output payload
  materialization, rehydrate staged-path markers in their diagnostic streams,
  and replay byte-exact status/diagnostics without writing a sibling success
  artifact to the destination.
- Review follow-up: access checkpoints use timestamp-only WAL touches that
  merge into the newest row, and cold commits merge durable output metadata
  plus sibling verdicts before publication.
- Repo hygiene: request validation, Dylint/time-macro setup, include discovery,
  and invocation parsing moved into `pipeline/request_prep.rs`; both pipeline
  modules are below the 1,000-line source ceiling.
- GREEN: 104 active artifact tests (1 ignored), 449 depgraph tests, and 766
  active daemon-core tests (25 ignored), plus focused merge/gating/loader
  regressions, formatting, diff checks, and warnings-denied Clippy.
- Pending: Unix ignored integration proof.
- clud-review: clean after cached-error rehydration and panic-free split
  follow-ups (one reviewer).

# #1402 eliminate request-time probe hashing

Issue: https://github.com/zackees/zccache/issues/1402

- [x] Publish running-process 4.10.5 with cached legacy SHA-256 identity and
  pre-4.10.4 identity-file compatibility.
- [x] Upgrade zccache and encode probe identity from the daemon's cached digests.
- [x] Prove probe responses succeed after the recorded executable path disappears.
- [x] Run focused tests, formatting, affected-target checks, and Clippy.
- [ ] Run the review gate, push the rewritten PR, merge, and close #1402.

## Review

- RED: running-process 4.10.4 rejects a 4.10.3 daemon identity JSON because
  its new `legacy_exe_sha256` field lacked a serde default, so zccache cannot
  establish the stale instance identity required for safe replacement.
- GREEN: published running-process 4.10.5 reads the pre-4.10.4 identity fixture;
  the compatibility decoder still reads historical protobuf field 3, while the
  response succeeds with a deliberately nonexistent `exe_path`.
- GREEN: all 53 IPC tests and the feature-complete 259-case CLI suite pass;
  affected IPC, CLI, and daemon all-target Clippy passes under the repository
  policy, and formatting plus whitespace checks are clean.
- Repo hygiene: the pre-existing CLI runtime tests moved to `runtime/tests.rs`,
  leaving the production lifecycle module at 771 lines.

# #1422 bound Tokio Console integration startup realistically

Issue: https://github.com/zackees/zccache/issues/1422

- [x] Capture the hosted RED and compare it with recent green duration evidence.
- [x] Keep daemon startup waits bounded while allowing loaded-runner variance.
- [ ] Run the focused test, review gate, hosted Integration rerun, merge, and close.

## Review

- RED: hosted Integration run 32388028985 timed out after 10 seconds while the
  dormant daemon had logged startup and resolved cache state but had not yet
  emitted its listening marker.
- GREEN target: use one named 30-second startup budget for dormant and active
  daemon log readiness while retaining the short negative TCP probe.

# #1423 serialize cargo-registry test environment

Issue: https://github.com/zackees/zccache/issues/1423

- [x] Reproduce the cache-layout failure in the default parallel CLI suite.
- [x] Serialize cargo-registry test environment reads and writes.
- [x] Restore overrides during unwinding and cover the panic path.
- [x] Run the full CLI suite, formatting, Clippy, and the review gate.
- [ ] Push, merge, close #1423, and verify hosted CI.

## Review

- RED: the default parallel CLI suite observed the real cache root while a
  sibling test temporarily installed a test `ZCCACHE_CACHE_DIR`; 258 passed,
  one failed, and two were ignored. The failing assertion passed alone.
- GREEN: one module-wide lock covers both environment variables and every
  participating read; an RAII guard restores prior values during unwinding.
- Focused GREEN: all four cargo-registry tests pass with 16 test threads.
- Full GREEN: 258 CLI tests pass with two ignored; formatting, diff checks,
  and repository-policy Clippy pass with only three unrelated baseline warnings.
- clud-review: clean (one reviewer).

# #840 retire the bincode IPC body

Issue: https://github.com/zackees/zccache/issues/840

- [x] Add RED coverage for rejecting retired full-family wire values.
- [x] Land compatibility fallback before changing the full-family default.
- [x] Flip auto/unset full-family requests to prost while preserving explicit overrides.
- [x] Add daemon telemetry for remaining bincode traffic and expose it in status.
- [ ] Run the sanctioned performance matrix and full correctness/review gates.
- [x] Remove the retired IPC lane and flatten dispatch after the public-release soak.
- [ ] Push, merge, close #840, and verify hosted CI.

## Review

- Slice 1 (complete in PR #1312): ExecProbe/ExecStore have real protobuf
  messages and exhaustive conversion coverage.
- RED: a fake bincode-only daemon rejects the first prost frame and requires
  the exact request to arrive once on a new bincode connection.
- GREEN: unset/auto full-family callers prefer prost and retry only a structured
  protocol-version rejection; application errors, ambiguous closes, and I/O
  failures never replay compile/link work. Explicit `prost`, `bincode`, and `frame`
  selections remain strict.
- Telemetry: the daemon counts decoded legacy requests by bounded request ID;
  prost/human/JSON status expose the map, while a byte-for-byte compatibility
  test proves the bincode `DaemonStatus` shape is unchanged.
- Focused GREEN: protocol selection and bincode-shape tests, IPC old-daemon
  fallback, wrapper retry policy, and live mixed-wire daemon status all pass.
- Remaining gate: commit the finalized tree and run the clean-checkout
  sanctioned performance matrix on a quiet host.
- Final clud-review: clean after structured-only control fallback, telemetry
  availability, no-replay retry gating, source splits, and directory READMEs
  (one reviewer).
- Review follow-up: full-family failures now retain connect-versus-delivery
  phase, so idempotent session-end suppresses only a proven pre-dispatch
  unreachable daemon. A peer close after receiving the request propagates.
- Focused GREEN: the classified connect failure and Windows post-dispatch
  close regressions pass on the normal Soldr wrapper.
- Final review follow-up: the phase API moved into documented `full_family.rs`
  to keep `lib.rs` below 1,000 lines; migration-era protocol docs now describe
  the live non-streaming, control, and streaming prost-first paths accurately.
- clud-review: clean after the phase-aware session-end repair and source split
  (one reviewer).
- Release-cycle constraint: the compatibility lane cannot be deleted until a
  shipped release reports zero bincode traffic for the required soak period.

# #1417 never replay compile/link after ambiguous delivery

Issue: https://github.com/zackees/zccache/issues/1417

- [x] Identify every outer retry and responsive-wedge replay after request send.
- [x] Restrict transport recovery to proven pre-dispatch failures.
- [x] Keep responsive-daemon no-kill behavior while failing the ambiguous request.
- [x] Add RED/GREEN coverage for pre-dispatch retry and delivery-unknown refusal.
- [ ] Run focused/full correctness and review gates, merge, and close #1417.

## Review

- RED: `link_with_retry` treated `DeliveryUnknown` exactly like connect failure,
  and all three responsive-wedge branches sent a second compile/link while the
  original could remain queued or executing.
- GREEN: only `PreDispatch` failures recover; send/receive/wedge outcomes are
  terminal after delivery becomes ambiguous. A responsive daemon is preserved
  without replaying the current request.

# #1025 restore current C++ performance gates

Issue: https://github.com/zackees/zccache/issues/1025

- [x] Add a focused regression proving cached response-file expansion is not
  deep-cloned again while preparing effective compile arguments.
- [x] Preserve response-file mutation invalidation and nested dependency checks.
- [x] Attribute the current compiler-driver cold-link gap from hosted miss profiles.
- [x] Implement only measured zccache-owned hot-path reductions without changing APIs.
- [ ] Run focused correctness, unchanged performance gates, review, merge, and close.

## Review

- RED: hosted Perf Guard run 32395073944 reports single-file response-file warm
  at 1.417x versus sccache (1.50x required) and compiler-driver cold at 0.819x
  versus bare clang++ (0.85x required).
- Source trace: response-file hits clone the cached ~283-string expansion, then
  `effective_compile_args` deep-clones that vector again before request-cache
  lookup even when no path-remap flags are injected.
- Driver-link attribution: hosted attempts spent 7.6–8.1 ms discovering the
  tool/input hashes and another ~4.6 ms outside the compiler and named phases.
- GREEN: effective argument preparation now consumes the cached expansion and
  moves its string buffers through unchanged when no maps are inserted. Top-level
  and nested response-file mutation regressions remain green.
- GREEN: cold non-archive links prepare their staged output plan and read-only
  side-effect snapshot alongside hash discovery. The compiler still starts only
  after a proven cache miss, and warm metadata-cache hits retain the existing path.
- Native diagnostic: driver-link unaccounted time fell from ~22 ms to ~9 ms;
  warm driver hits remained ~3 ms. Hosted Linux remains the timing authority.

# #1428 fix stale test package commands

Issue: https://github.com/zackees/zccache/issues/1428

- [x] Inventory all 38 stale `zccache-daemon` package examples under `crates/zccache/tests`.
- [x] Replace them with the current `zccache` package name without changing daemon binaries.
- [x] Prove the corrected focused command resolves.
- [ ] Merge and close #1428.

## Review

- RED: `soldr cargo test -p zccache-daemon ...` fails because the package was
  renamed to `zccache` by #365.
- GREEN target: every affected test comment and README uses `-p zccache`, and
  the focused performance-test target compiles through the documented command.
- GREEN: Cargo metadata resolves `zccache` and its `perf_bench_test` target;
  the corrected no-run command reaches dependency compilation instead of
  failing package selection, and the stale package string inventory is empty.

# #1432 restore immediate Rust workspace-link cache hits

Issue: https://github.com/zackees/zccache/issues/1432

- [x] Add a RED regression for a staged payload whose durable publication is still blocked.
- [x] Publish an owned provisional artifact without weakening conflict handling.
- [x] Run focused correctness and performance coverage.
- [ ] Run review, push, merge, and verify the hosted Rust speed floor.

## Review

- Hosted RED: main Perf Guard runs 32415853442 and 32419543154 each reproduced
  three warm workspace-link misses after an exact five-second pending wait.
- Root cause: detached Rust staged publication keeps the private files alive but
  withholds the in-memory artifact until copy/sync/hash publication completes.
  The next proven depgraph hit times out and recompiles the same staticlib.
- RED: once a provisional path was visible, the normal cache-blob delivery path
  rejected it because compiler-private files do not yet have durable digest
  registration; an in-flight hit could also outlive the artifact clone that
  owned its staging plan.
- GREEN: vacant keys publish a path-backed provisional artifact immediately.
  Its materialization guard owns the private plan through byte delivery and
  uses independent reflink/copy without durable-blob verification. Publication
  failures remove only their own provisional instance.
- GREEN: the eight-test staged-publication module and the independent ownership
  regression pass. The real ignored `perf_rust_workspace_link` fixture records
  one cold miss followed by five cached warm trials (21 ms median locally).

# #1435 preserve endpoint context on immediate connection failures

Issue: https://github.com/zackees/zccache/issues/1435

- [x] Reproduce the rust-plan session-stats JSON regression locally.
- [x] Add shared transport coverage for an immediate connection failure.
- [x] Preserve the endpoint and original I/O error kind at the transport boundary.
- [ ] Run focused correctness, review, merge, and close #1435.

## Review

- RED: `rust_plan_session_stats_lookup_errors_surface_in_json` loses the endpoint
  after session stats moved to the shared full-family roundtrip in #1425.
- Root cause: transport timeouts add endpoint context, but immediate platform
  connection errors are propagated raw.
- GREEN: the shared transport adds endpoint context while preserving the I/O
  kind and original error source; all 58 IPC tests and the unchanged rust-plan
  JSON regression pass.
- clud-review: clean after retaining the original platform error in the source
  chain (one reviewer).

# #1429 restore coverage llvm-profdata

Issue: https://github.com/zackees/zccache/issues/1429

- [x] Confirm the repeated hosted failure occurs after all unit tests pass.
- [x] Trace the missing binary to a component installed after solo-cache restore.
- [x] Opt Coverage out of the incompatible solo toolchain cache.
- [ ] Merge, observe a successful hosted Coverage run, and close #1429.

## Review

- RED: main runs 32415853396 and 32419543131 report
  `llvm-tools-preview` current, then fail because `llvm-profdata` is absent.
- Root cause: setup-soldr documents that components installed after its action
  are not part of the solo snapshot and explicitly directs such jobs to opt out.
- GREEN target: a hosted warm Coverage run generates `lcov.info` and reaches a
  successful Codecov upload without adding LLVM tools to unrelated jobs.

# #1433 publish the Python exec_cached API

Issue: https://github.com/zackees/zccache/issues/1433

- [ ] Add RED contracts for daemon-restart persistence and the public Python API.
- [ ] Persist ExecProbe/ExecStore bytes in the normal namespaced KV store.
- [ ] Expose `zccache.exec_cached(...)` with GIL-free IPC and exact bytes replay.
- [ ] Gate release wheels on Linux, macOS, and Windows behavior smoke tests.
- [ ] Run focused/full correctness and review gates, merge, and close #1433.
- [ ] Publish the API and replace FastLED's bespoke AST cache with timing evidence.

## Review

- Key compatibility: retain `zccache-exec-probe-v1`; persistence changes only
  where the already-derived 32-byte key stores its opaque result bytes.
- Failure policy: daemon/protocol/store failures surface to Python and never
  invoke an uncached fallback behind the caller's back.

# #1439 stop ARM64 xwin C++ mode from breaking ring

- [x] Reproduce RED: auto-release run 32446557448 fails in `ring` with
  `void *` init and `-Wc++11-narrowing` errors caused by global `CFLAGS=-TP`.
- [x] Root-cause: the language mode can only be chosen package-scoped, inside
  `mimalloc-pprof`'s own build script. `CFLAGS` reaches every `cc-rs` build
  script, so no consumer-side flag can fix this without breaking `ring`.
- [x] Confirm upstream already fixed it: zackees/mimalloc-pprof#224 selects
  clang's C11 `stdatomic` for `aarch64-pc-windows-msvc` and bumped to 0.9.3 --
  but 0.9.3 was never tagged, so crates.io still tops out at 0.9.2.
- [x] Drop the global `-TP` injection from the xwin ARM64 lane.
- [x] Raise the workspace `mimalloc-pprof` floor to 0.9.3.
- [x] Add contract tests: no global `/TP`/`-TP` in the action, and a locked
  `mimalloc-pprof` >= 0.9.3.
- [x] Upstream docs PR zackees/mimalloc-pprof#228 records the cross-compilation
  contract in both READMEs.
- [x] Merge #228, tag `v0.9.3`, and let auto-release publish to crates.io.
- [ ] `cargo update -p mimalloc-pprof` here, run validation, open/merge the
  zccache PR, close #1439, resync `main`.

## Review

The `-TP` in the ARM64 xwin lane was never the fix -- it was a consumer trying
to make a package-scoped decision with a process-global lever. `CFLAGS` is
read by every `cc-rs` build script in the graph, so forcing mimalloc's C++
atomics path also compiled `ring`'s C sources as C++ (`void *` initialisation
errors, `-Wc++11-narrowing`) and stopped the 1.13.6 release before it
published anything.

The choice belongs in the crate that owns the source. zackees/mimalloc-pprof
had already made it (#224: select clang's C11 `stdatomic` for
`aarch64-pc-windows-msvc`, released as 0.9.3 in the changelog) -- but `v0.9.3`
was never tagged, so crates.io topped out at 0.9.2 and no downstream lockfile
could reach the fix. Tagging it was the actual unblock.

Landed here:

- deleted the `Configure xwin ARM64 C compatibility` step, so no global
  language-mode flag reaches any `cc-rs` dependency
- raised the workspace `mimalloc-pprof` floor to `0.9.3` and relocked
- two contract tests: the action carries no `/TP`/`-TP` and declares no such
  step; the locked `mimalloc-pprof` is >= 0.9.3

mimalloc stays the global allocator on every shipped target, Windows ARM64
included. Upstream `build (aarch64-pc-windows-msvc)` passed on #228, which is
the direct evidence that 0.9.3 cross-compiles for that target.

Upstream follow-through: #227 (my duplicate) closed against #223; #228 merged,
documenting the cross-compilation contract and why `CFLAGS=-TP` is the wrong
lever, in both the crate and repo READMEs.

# Rust/C amalgamation compile admission

- [x] Add RED coverage for the published zccache crate as a Rust amalgamation.
- [x] Give known Rust amalgamations and oversized C translation units exclusive compiler access.
- [x] Prove ordinary compiles remain concurrent and cache hits skip the barrier.
- [x] Run focused tests, formatting, Clippy, full validation, and review.
- [ ] Merge and publish the next zccache release.

# Current-main performance regression profile

Issue: https://github.com/zackees/zccache/issues/1511

- [x] Audit the sanctioned benchmark, existing #1036/#1099 campaign, and open profiling work.
- [x] Add a bosn-managed Linux stack for the existing benchmark with persistent soldr/target caches.
- [x] Capture current-main wall-time, on-CPU, off-CPU/syscall-wait, RSS, heap, and Massif evidence.
- [x] Rank the top three owned slowdowns and file the evidence-backed implementation issue.
- [x] Add RED performance tests, implement the fixes, and capture paired GREEN evidence.
- [x] Attempt the full sanctioned repeat-5 matrix and retain every clean or invalid sample.
- [ ] Merge, close the issue, and restore clean `origin/main`.

## Review

- Daemon identity now computes only BLAKE3, using its memory-mapped parallel
  reader; the required legacy SHA-256 slot stays zero. Bounded Linux Callgrind
  moved `current_backend_identity` from 56.59% of instructions to 0.05%, while
  heaptrack moved the executable-sized allocation from 57.37 MiB to a 1.05 MiB
  peak heap.
- Ordinary Clang C++ misses now consume the compiler's exact `-MD/-MF`
  dependency file instead of paying for public `-H` stderr tracing. The clean
  50-file cold benchmark improved from 58.515 s to 16.795 s; its compiler-span
  ratio is 1.064x bare, below the 1.25x target. The ignored system-header
  mutation test passes under Bosn Linux.
- Interactive Rust link-like work uses the existing Auto priority policy
  instead of unconditional Low priority. Clean final observations were 7.620 s
  for build mode and 5.377 s for check mode, with explicit foreground
  materialization timing and effective Normal priority in every serial row.
- The sanctioned harness had silently continued to build registry zccache
  1.13.11 after Soldr externalized the dependency. It now patches the exact
  read-only checkout and rejects a lockfile that still names a registry source;
  all 52 focused harness tests pass.
- The exact-source medium `cold-tar-untar-warm` cell passed all five clean
  samples: cold median 95.834 s (MAD 2.709 s, 87.403-106.237 s), warm median
  13.747 s (MAD 0.244 s, 13.381-13.996 s), with 141 hits, zero misses, and no
  aborts, retries, or daemon fallbacks in every sample.
- A clean exact-source repeat-5 attempt completed the medium worktree cell:
  cold median 96.791 s (MAD 3.223 s, 88.017-130.487 s), warm median 14.013 s
  (MAD 0.113 s, 12.947-14.126 s). The overall matrix still failed 7/8 cells.
  One medium cold sample was invalid because `tokio`'s rustc was killed by
  signal while cgroup `memory.events` reported no OOM. The remaining failures
  were current Soldr/Cargo lifecycle ceilings despite cache success: sqlite
  warm phases took 55-69 s with 100% zccache hits. No threshold was widened,
  and the invalid sample is not claimed as performance evidence.

# Cache-hit blocking offload

Issue: https://github.com/zackees/zccache/issues/1578

- [x] Carry the embedded host runtime into shared request state.
- [x] Offload compile, exec, and link hit lookup, staged-store locking, planning, and writes.
- [x] Preserve per-session staged telemetry across the blocking boundary.
- [x] Attribute staged-lock time before materialization begins.
- [x] Prove a blocked lane does not stall a one-worker host runtime.
- [x] Run focused tests, Bosn full tests, Bosn lint, and review.
# Windows long-path cache-hit materialization (Soldr #3330)

- [x] Preserve the real Windows RED evidence: Soldr's fresh-worktree `.rmeta` hit fails after successful staged publication; lifecycle reports destination-write failure with no OS errno.
- [x] Add a deterministic cache-hit regression for hardlink-registration failure and keep the safe copy tier observable.
- [x] Make a failed hardlink registration fall back to an independent copy with an explicit warning; retain digest verification and all hit-output assertions.
- [ ] Run focused tests, formatting, warnings-denied checks, review, then push a focused PR and verify Windows plus downstream Soldr replay before release/pin/closure.
# #1749 rustc OUT_DIR cross-worktree reuse

- [x] Capture the baseline A/B artifacts and prove which include-only outputs are byte-equivalent.
- [x] Add a focused RED test for an include-only `OUT_DIR` compile across two roots, plus negative tests for an embedded path and changed generated file.
- [x] Implement a fail-closed proof/opt-in path that restores safe hits without dropping general rustc env-dep soundness.
- [ ] Confirm the sqlite-link worktree fixture's hits, run the eight-cell Docker matrix and relevant Rust/Windows checks.
- [ ] Review, push, validate, and merge a PR that resolves #1749.

# #1752 Soldr-prefixed benchmark stats

- [x] Add a RED parser regression for elapsed-time and ANSI-prefixed multi-language tables.
- [x] Normalize only supported log prefixes and make the regression GREEN without changing unprefixed parsing.
- [x] Parse the September 27 uploaded log end to end, render all four images, and verify provenance.
- [ ] Run focused CI tests and lint, review, open/validate/merge the PR, then dispatch and verify a fresh benchmark-stats run on main.

# #1754 typed benchmark metrics and truthful charts

- [x] Add RED contract/renderer tests for the documented failures, a September 28 real-log excerpt, and publication fail-closed behavior.
- [x] Emit versioned typed benchmark records from all registered Rust perf tests, with exact nanoseconds, cache bytes, identity/methodology, and sccache evidence; use validated Python dataclasses internally and JSON only on the wire.
- [x] Validate a complete run before publication; preserve legacy Markdown consumers and historical schema compatibility.
- [x] Render warm-only and methodologically distinct rows honestly, retain per-mode cache sizes, and prove legible final-resolution images.
- [ ] Run targeted and broad tests, review, rebase onto origin/main, push a PR, wait for CI, merge, and remove this worktree/branch leftovers.


# #1885 Bosn Actions isolated test gate

- [x] Measure unchanged Integration and MSRV workflows using published Bosn 0.1.12 / act2.3 (499.122 s and 147.414 s, both passed).
- [x] Add failing source-proof regressions before replacing the legacy task.
- [x] Replay MSRV in check and Integration in tests; require clean source plus successful executed steps.
- [x] Update gate/drift/docs contracts; retain native, ignored/stress and other workflow coverage.
- [x] Run focused tests, cache-footprint guard, review and the complete committed-tree local gate before push (#1886; 637 s; all nine remote workflows passed first push).
- [ ] Separately migrate the ACT-specific cache writer overrides through the published remote-permission contract.


## #1885 remote cache permission slice

- [x] Add RED regressions for remote PR-write overrides, unsupported action refs, ACT-only inputs and explicit global disables (6 RED; 82 targeted tests GREEN).
- [x] Use published setup-soldr global auto / guarded remote inputs in all 23 existing workflow writers; preserve explicit read-only consumers and measured family budgets.
- [x] Update the footprint guard, delegated build action, tests and documentation without adding workflow files; split writer-barrier coverage below the source-file size ceiling.
- [ ] Run targeted tests, footprint guard, primary review and the complete committed-tree gate; verify real local/remote save decisions before merge.
- [ ] Follow up isolated compiled-output publication / assertion-only failure reuse from measured evidence; preserve CACHE-007/008 payload and failure constraints.


## #1885 Integration isolated compile-store publication

- [x] Confirm #1867/#1868 already provide the publisher and a measured Linux Test precedent.
- [x] Add a failing Integration wiring regression; retain guarded remote saves and job-status gating.
- [x] Give Integration its own profile key and publish after shutdown plus audit without changing suite coverage.
- [x] Measure the naive isolated publisher: cold 913 s passed; build 1,056,628,741 bytes, cook 3,533,586,676 bytes. Reject the added cook archive.
- [x] Preserve warm failure: 495 s, prebuild 285→140 s, 3477/3478 passed; durability flush 786 ms exceeds unchanged 500 ms limit. Failed-job save gate refused publication.
- [x] Remove Integration setup cook; restore dependencies through the isolated unit store. Reserve 1.2 GB for it within the build family, keeping total 9.5 GB unchanged; explicit ci-tests protects forbidden products.
- [ ] Measure the revised committed tree and confirm coverage plus cache decisions; do not treat the failed warm run as an attestation.
- [ ] Run focused checks, primary review and the full committed-tree local gate before pushing; verify the first remote head.

- [x] Validate committed no-cook candidate e3b4520: full gate passed in 954 s, stamped e587de8e; all 3478 tests passed. Explicit ci-tests default serialized nextest, so restore num-cpus parallelism in the final slice.
- [x] Add RED old-lock retention/bootstrap checks; retire measured Test and Integration generations, reserve the initial 1.2 GB without double-counting, and replay the captured 7.90 GB inventory at a 9.10 GB forecast below the unchanged 9.20 GB target.
- [ ] Review and validate the final committed planner/parallelism slice before push.
# Shared CI compiler-store snapshots (ci.yml#362)

- [x] Observe an isolated RED test retaining multi-output metadata and verdicts.
- [x] Export staged-v2, pack and flat payloads using the existing layout resolver.
- [x] Reject corrupt payloads and preserve prior committed snapshots (isolated test).
- [x] Verify compatibility on import before any destination publication (isolated test).
- [x] Import leaves its immutable source unchanged and rejects undecodable indexes.
- [x] Two concurrent publishers install one complete, importable snapshot.
- [x] Offline export excludes active writers using the daemon's canonical root lease.
- [x] Qualify the thin `zccache cache export/import` CLI and its schema-1 output.
- [ ] Run focused isolated tests, broader gates, and local review before pushing.
- [ ] Integrate the backend API with cache transport; prove fresh-engine reuse.

Current validation: intentional RED controls for unimplemented export, mixed
legacy stores, immutable import, strict index decode and active-writer exclusion.
The corrected writer-exclusion run passed all 127 artifact tests (one ignored),
six canonical writer-lease tests, and warnings-denied artifact Clippy inside
the existing Bosn gate stack. Timestamp/read-only retention is covered.
The corrected CLI run passed export/import in one process, wrong-context refusal,
and warnings-denied Clippy for artifact, core and CLI crates. The focused run
took 23.42 seconds inside the existing isolated stack; this is diagnostic, not
a controlled performance comparison. Full repository qualification and review
remain pending; this unpublished draft is not fresh-engine cache-hit proof.

The sole primary review found that a missing staged pointer on import reached
legacy resolution and could write a source lifecycle event. The isolated RED
reproduced that mutation with legacy validation enabled (127 passed, one failed,
one ignored). Import now requires staged generations through the shared copier;
export alone normalizes legacy layouts. The added regression also refuses a
healthy legacy fallback, preserving the schema-1 transport boundary. Full gate
attempt on 78fd40e8 passed lint and Python lanes; its MSRV replay was deliberately
cancelled for this source correction and no complete gate pass is claimed.

The review correction passed 128 artifact tests (one ignored), six writer
lease tests, one CLI test and focused warnings-denied Clippy in isolation.
The same primary reviewer confirmed the fix and returned clean. Full repository
qualification remains required before publication.

# PCH private input views and FastLED integration

- [x] Reproduce shared timestamp replay hiding an edit from an independent Ninja build.
- [x] Replace shared timestamp writes with verified private snapshots and preserve include aliases.
- [x] Freeze chained producer imports and bind cached companion provenance to the exact bytes.
- [x] Validate FastLED through an editable install: 320 native unit targets, 96 examples, focused PCH checks, lint, and a real WASM build.
- [x] Make real modern-Clang regressions mandatory in the existing isolated integration workflow.
- [x] Reproduce the remaining legacy chain defect: a same-stat base replacement plus header repair compiles inconsistent values.
- [x] Refuse private views and producer freezing without recursive bound provenance; retain unchanged legacy consumers. The actual overlay-boundary wrong-code test failed before the fix and all 11 provenance tests pass afterward; primary review is clean.
- [x] Fix the generic-exec lookup/acquire race exposed by full integration: after a peer publishes and drops its slot, a late new owner rechecks the cache. The deterministic regression failed before the fix; five coalescing unit tests and 27 generic-exec integration tests pass afterward. The one-spawn assertion remains unchanged.
- [ ] Review the correction, rerun focused regressions and the full clean-tree local gate.
- [ ] Merge and publish zccache 1.15.6 through the existing release workflow.
- [ ] Replace FastLED's editable override with the published package, verify, and merge removal of local timestamp workarounds.

# Bounded synchronization and async state (soldr#3836 / #3845)

- [x] Inventory recent soldr PRs and compare the vendored fixes with this workspace.
- [x] Fix compiler identity probes holding spawn admission while waiting.
- [x] Introduce bounded synchronization/wait facades; keep data guards out of async code.
- [x] Enforce the boundaries with resolved-symbol Dylint and compile-fail fixtures.
- [x] Migrate affected callers and validate concurrency regressions and lint wiring.
- [x] Document guarantees, exceptions and the upstream PR inventory.

Validation: 1,287 isolated Rust tests and two compile-fail doctests passed across
`bounded-sync-test` and `bounded-sync-contention-test`; all three new Dylint UI
fixtures passed. Workspace Clippy passed with warnings denied. Full Dylint
passed Linux, Windows MSVC and macOS source selections; wiring, cache-footprint,
and kernal-api inventory checks passed. Host compiler checks used
`SOLDR_RUSTC_WRAPPER=none` to diagnose/bypass the installed soldr 0.9.35 stall;
isolated runtime tests used the normal soldr wrapper.

## Soldr mandatory-timeout audit

- [x] Compare soldr's complete raw-wait denylist and timeout-only wait facade.
- [x] Add the missing bounded channel send, preserving unsent values.
- [x] Remove nonexistent replacement suggestions from lint diagnostics.
- [x] Reject kernal-api's optional-timeout command-runner bypass; move both product callers behind the required-Duration facade.
- [x] Document optional IPC/download budgets and missing hold watchdog as remaining gaps, rather than claiming all async waits are bounded.
- [x] Verify audit changes: workspace/all-target compilation; all 11 Dylints across Linux, Windows and macOS source selections; 3 lint UI fixtures; isolated bounded-send regression; architecture and lint wiring checks.

# Sequential implementation of meta issue #1960

Use this checkout only. Complete and merge each child PR before starting the
next child, then sync/rebase against origin/main and leave a clean tree.

- [x] #1961 bounded state/probe facade migration: merged PR #1972; full six-lane gate passed. Synced/rebased main to origin/main (25d71f09), clean tree before #1962.
- [x] #1962 holder watchdog and diagnostics: merged PR #1973 after all six gate lanes passed (1,394s); rebased clean main to origin/main 0fb36402.
- [x] #1963 bounded generic process liveness (merged PR #1974).
- [ ] #1964 detach staged deletion from publication exclusion.
- [ ] #1965 durable-entry payload demotion.
- [ ] #1966 default eligible native/link/exec staging.
- [ ] #1967 complete timeout/feature enforcement.
- [ ] #1968 HTTP budget owner and Dylint.
- [ ] #1969 canonical owned environment flags.
- [ ] #1970 retained helper diagnostics and Dylint.
- [ ] #1971 terminating test budgets and owned test children.

#1961 review found and corrected blocking Barrier writer starvation, a staged
weak-slot mutex held over a longer file-lock wait, and download feature guard
lifetimes. The probe regressions were replayed against the actual HEAD probe
functions (only spawn_shared visibility adapted to compile the old path) and
both failed with the expected held-admission assertion. Restored current code
before GREEN validation. The Barrier and weak-slot regressions likewise failed
before their repairs. Full clean-tree qualification and publication remain open.

Final primary review is clean. Probe tests (2), queued blocking Barrier writer
(1), and staged weak-slot scope (1) all have focused RED -> GREEN evidence.
Explicit download-daemon Dylint initially rejected attachment guards and now
passes with owned job snapshots; membership/client-count ordering is preserved.
The daemon-only Clippy selection exposed an existing unused client-connect
warning, so final Clippy validation selects the combined product feature set.

The first full clean-tree gate passed lint, Python, MSRV, docs and cross-OS
Dylint lanes, but integration exposed two regressions: weak gate ownership
allowed overlapping link operations, and concurrent staged initialization took
duplicate OS leases. Opaque permits now retain the outer gate owner; a separate
data-free initialization gate coalesces staged leases outside the state mutex.
The weak-owner test failed before the repair; core synchronization tests and
both unchanged integration assertions pass afterward. The subsequent six-lane gate passed in 1,420s; PR #1972 merged and closed #1961.

The correction gate passed all five non-integration lanes. Integration setup
repeatedly failed before tests on GitHub anonymous latest-release rate limits.
Pin setup-uv in the existing CI/integration workflows to verified uv 0.12.13
(the host version), removing the latest-release lookup without skipping tests.
All six lanes subsequently passed on that tree before PR #1972 was published and merged.

## #1962 implementation plan

- [x] Capture subprocess RED for wedged State callbacks and native holders, retaining libtest capture and bounded process/output ownership. All three timed out against the previous owners.
- [x] Add independent RAII waiter/holder registry and watchdog with caller/thread diagnostics; report long waits, terminate overdue short holds. Three real-child regressions now terminate with visible diagnostics.
- [x] Integrate State and native guard owners, preserving poison and cancellation; separately budget spawn/materialization holds and exclude long data-free operations.
- [x] Verify spawn/materialization subprocess failures, cancelled/dropped registration cleanup, recoverable acquisition, legitimate long operations and writer fairness.
- [x] Extend deny-level Dylint/UI enforcement for the private watchdog owner and wrapper guards; update architecture guarantees and limitations.
- [ ] Run focused RED -> GREEN, review, full clean-tree gate, merge separate PR, then rebase main to origin/main.

#1962 first implementation evidence: eight isolated watchdog tests pass,
including real blocked async/sync callbacks, a native holder, cancelled waiters,
poison/drop cleanup, report-only overdue queues and a real operation permit
held beyond the short-state budget. Core/daemon all-target Clippy with
`test-support` passes with warnings denied. Depgraph persistence uses a data-free
operation gate because its scope includes disk I/O. Spawn/materialization
subprocess coverage, lint fixtures, docs, review and full gate remain pending.

#1962 complete focused evidence: 217 isolated core tests and 16 maintenance
schedule tests pass. Actual spawn/materialization subprocesses terminate at the
production 15s/120s limits with holder/waiter sites visible despite capture.
A real-core Dylint example compiled forbidden wrapper guards before the resolved
wrapper-type classification; it now rejects aliases, concrete wrapper fields,
unit native guards, UFCS and function-item acquisition bypasses. All three
existing UI fixtures and both dependency-bearing examples pass. Review,
workspace Clippy and full clean-tree qualification are next.

#1962 primary review found two HIGH issues; both have new focused RED -> GREEN
regressions: unread/full stderr blocked termination, and real spawn/copy admission
aborted after 15s during a legitimate 120s-budget copy. Diagnostic delivery now
uses one bounded writer and at most 250ms before termination; real admission
uses the operation queue budget, with unchanged 15s/120s hold limits. Twelve
final watchdog tests, legitimate-copy regression, all five UI fixtures and
workspace/all-target Clippy with cli+download-daemon features pass. Correction
review is clean (one primary reviewer). Full clean-tree gate remains pending.

#1962 first clean-tree gate passed lint, Python, MSRV and docs, then exposed
Dylint6.0.3's example-runner dependency discovery requiring a bare rustc argv.
Normal soldr compiler wrapping prevents that parser from identifying the
compiler invocation. The harness now gets real-core link inputs from Cargo JSON
artifacts and runs both examples through the UI source runner, preserving the
managed nightly toolchain and wrapper. Five local UI fixtures pass; correction
review is clean. Full isolated normal-wrapper gate rerun remains pending.

## #1963 implementation plan

- [x] Capture RED: the live silent generic tool bypassed monitoring and exceeded15s; the shared loop now expires, reaps and releases its output gate.
- [x] Unify generic linker/exec/hook/passthrough sessions with the compiler progress/CPU owner; require a concrete non-optional liveness policy at process facade entrypoints.
- [x] Preserve progressing child behavior, concurrent output drains, finite descendant-pipe grace, typed I/O failures, cancellation and bounded kill/reap ownership.
- [ ] Extend deny-level resolved-symbol process-owner enforcement and positive/negative fixtures for semantic kernal-api APIs and aliased/UFCS/function-item bypasses.
- [ ] Update authoritative docs; focused RED -> GREEN, review and full clean-tree gate; merge separate PR then rebase main to origin/main.

#1963 first runtime implementation: all 31 call sites now supply a required
ProcessLiveness value; stall/runtime/cleanup are finite positive budgets,
post-exit drain is a Duration (zero explicitly abandons, never selects EOF).
Configured policy preserves 300s stall/2s drain with a finite 6h total cap for
progressing/CPU-unobservable children and 5s cleanup. The canonical owner applies
the total across spawn and run, and confirms kill/reap on runtime expiry.
Generic and compiler children share CPU/output monitoring and concurrent drains;
read errors return through bounded cleanup. Initial CPU-progress testing exposed
an unchanged-output timestamp causing false termination after a short quiet CPU
sample; CPU advances now refresh the shared progress clock (focused RED -> GREEN).
Seven isolated focused tests pass, including progress, hard deadline and
cancellation. All-target daemon compilation and focused Clippy passed before the
last timestamp correction. Existing process/coalescing regression checks are
running. Process-owner Dylint, remaining fault/backpressure coverage, authoritative
docs, review and full gate remain pending. Exec coalescing helpers were split
without behavior changes so the modified handler stays below 1,000 lines.

#1963 backpressure RED -> GREEN: a live receiver with a full output channel
previously suspended the process monitor beyond a two-second outer test budget.
Forwarding now requires an explicit send Duration (the session stall budget),
returns TimedOut on backpressure, and routes that failure through owned cleanup.
All 62 isolated process tests and five exec-coalescing tests pass. Semantic
process-owner Dylint compiles; real-dependency UI validation is in progress.

The broader CPU-progress regression exposed running-process 4.10.16's Linux
CPU Duration conversion dropping fractional CPU seconds (also present on upstream
main). Keep native identity-bound session sampling/termination and supplement
progress with the existing kernal-api raw CPU ticks for the live owned tree.
This preserves observable subsecond CPU activity without a dependency patch or
PID-based termination. Captured failures remain in the local regression logs.

#1963 validation: 65 isolated process tests and five coalescing tests pass,
including live-consumer backpressure, pending-admission timeout cancellation,
and injected kernel stdout-fault cleanup with a real child and gate. The
public fault value is injected after child readiness, not claimed as an OS
pipe-failure reproduction. Workspace Clippy (cli + download-daemon) passes.
Dylint passed all supported OS selections; final matcher additions covering
native kernal foreground/sync APIs are included in UI and the full gate.

## #1964 implementation plan

- [x] RED: both maintenance and Clear paused recursive deletion block a real unrelated publisher; workers are resumed/drained before assertions. Evidence: /tmp/zccache-1964-sweep-red.log (2 expected failures).
- [ ] Introduce typed detach/sweep ownership; rename to private restart-safe trash under short exclusion, sweep on the blocking lane after release.
- [ ] Cover generation replacement, retained readers, retries/crash recovery, correct reclaimed-byte accounting, symlink/reparse refusal, two-second maintenance deferral and bounded Clear acquisition.
- [ ] Include publication rollback/conflict/invalid-generation recursive cleanup, which remains under store exclusion beyond the producer's maintenance patch.
- [ ] Add resolved-symbol deny-level Dylint for destructive sweep capabilities and slow filesystem work under publication/store guards; real negative/positive alias/UFCS/function-item fixtures.
- [ ] Update docs; run focused tests, one local review and clean-tree full gate, merge separate PR; rebase main to origin/main and delete feature branch.

#1963 completed: PR #1974 merged as adf2c5e1300b16662261700af5e4f99fe347bc96;
issue closed. All six gate lanes passed in 1,618s, including 52 strict PCH
regressions and 3,661 main tests, final normal-wrapper Dylint on three OS
selections and planted violations. Attested head `7cb7a78a80bf`, tree `cef40a9e0946`.
Local review clean; ten focused liveness cases rerun independently. Main was
rebased to origin/main (0/0), verified clean, and feature branches removed.

#1964 runtime checkpoint (not ready to push):

- Both original paused-sweep RED cases are GREEN. The additional retained-reader
  conflict case was RED (resolved old path vanished), then GREEN: conflict
  retires the pointer immediately, attempts generation retirement only after
  dropping publication's native shared/key guards, and defers while readers
  remain. Exclusive orphan collection revalidates the current pointer.
- `DetachedStagedTrash` owns already-renamed paths; Drop does no filesystem work.
  The sweep checks one shared deadline across enumeration/deletion and retries
  durable leftovers. Only its recursive owner deletes staged trees. Final
  empty-trash-directory unlink takes a zero-wait native exclusive permit to
  avoid racing new detaches. Clear's prior clean layout is preserved.
- Publication rollback/conflict/invalid-generation cleanup and startup orphan
  cleanup use retirement rather than recursive deletion under native locks.
  Normal maintenance returns sweep work from the publication commit. Planning
  recursive scans run without a store guard. Clear's staged detach and sweep
  run on the blocking lane; its staged sweep follows publication-writer drop.
- 85 targeted regressions pass: 33 staged-store, 11 cached-artifact, 36 disk
  maintenance, 5 Clear-handler (including ignored cases). Evidence:
  /tmp/zccache-1964-maintenance-regressions.log. New cases include failed-delete
  recovery, expired-sweep recovery, linked/reparse-root refusal and no traversal
  through a trash-entry link. Windows junction fixture is present but only Linux
  runtime has run locally so far.
- Large source files were split: staged_store/publish.rs;
  disk_maintenance_commit.rs and disk_maintenance_runtime.rs. Modified owners
  are below 1,000 lines. Digest sidecar naming is shared with the sweep owner.

Remaining #1964 work:
1. Complete Clear's other cleanup: synchronous flat-file enumeration/removal,
   metadata/depgraph snapshots, meson tree and exec KV namespace still run
   under its publication writer (some on the async worker). Retire live names
   atomically before releasing exclusion, then sweep on an owned blocking lane;
   avoid erasing fresh post-Clear publications. Preserve WAL queue ordering,
   lookup/startup leases and cancellation ownership. Do not simply drop the
   writer and delete live paths. The current staged-only split is partial.
2. Include failed/deferred trash in accounted cache usage. Reclaimed bytes now
   use successful sweep unlink allocation and existing last-link/COW eligibility,
   but pending .trash is skipped by current live-key scans. Add targeted
   partial-failure/hardlink/COW accounting and real crash/restart recovery proof.
3. Add actual global-publication pause regressions for maintenance and Clear,
   generation replacement races and final two-second contention/Clear deadlines.
   Existing new pause tests prove store-lock release with a real publisher;
   existing Clear tests preserve hydration/lookup leases.
4. Implement/register deny-level semantic Dylint for destructive sweep ownership
   and forbidden slow filesystem/sweep work while publication/store guards live.
   Cover aliases, UFCS/function items and wrappers; use typed phase capabilities
   where liveness cannot be established. Preserve narrow justified owners;
   private compile staging cleanup is distinct from shared store retirement.
5. Update authoritative docs, final Clippy and cross-OS/feature lint coverage,
   one clud-review invocation, full clean-tree gate, separate PR/merge, then
   switch/rebase main onto origin/main and delete feature branches.


### #1964 Clear/KV ownership progress (2026-10-22)

- Added typed `DetachedKvNamespace`: namespace-gated rename, blocking sweep after
  gate release, durable `.retired` recovery, link/reparse refusal, allocated-byte
  last-link/COW accounting. Two isolated namespace recreation/reopen tests passed
  (`/tmp/zccache-1964-kv-green.log`); missing-API RED retained separately.
- Clear now moves its maintenance permit and retained SharedState into its owned
  blocking worker. Publication exclusion covers only logical reset, live-name
  retirement and ordered WAL Clear acknowledgement. Staged, flat, snapshots,
  meson and exec namespace cleanup follows after writer release. Snapshot live
  names are never deleted by the later sweeper. WAL flush failure is an error.
- Both new Clear regressions failed before this change: publication stayed
  excluded during KV sweeping, and cancellation released maintenance too soon
  (`/tmp/zccache-1964-clear-cancel-red-bounded.log`). All 87 focused staged,
  retained-reader, disk-maintenance and Clear tests passed with the owned worker
  (`/tmp/zccache-1964-clear-owned-green.log`). Strengthened seeded-KV rerun also
  passed all 87 tests (`/tmp/zccache-1964-clear-seeded-green.log`).
- Compatibility KV Clear now also retires before sweeping instead of recursively
  deleting under its namespace gate. Its isolated full KV regression rerun passed 29 tests, with one existing
  allocation-heavy test ignored (`/tmp/zccache-1964-kv-full-green.log`).
- The earlier remaining-work item 1 is now implemented, pending final review.
  Items 2–5 remain open; do not treat this checkpoint as completed #1964.


### #1964 pending-trash accounting and process recovery (2026-10-22)

- Captured accounting RED: maintenance reported only 17 pending-write bytes
  while a 16 KiB retired payload remained (`/tmp/zccache-1964-trash-accounting-red.log`).
- The physical scan now returns live eviction candidates separately from pending
  trash allocation; both share inode deduplication. Pending trash contributes to
  before/after usage and budget pressure, never becomes a live eviction key.
- Three focused accounting regressions pass: recovered durable trash, partial
  unlink failure retaining its allocated usage, and live/trash hardlinks counted
  once with zero promised free-space gain. Receipts:
  `/tmp/zccache-1964-trash-accounting-extended.log`.
- A bounded child actually exits with a detached token still live, skipping Rust
  destructors. Parent reopens the store, publishes the same key again, recovers
  trash, and verifies the replacement remains readable. Passed with seven other
  sweep regressions (`/tmp/zccache-1964-process-recovery.log`). The ignored child
  fixture is explicitly executed by its parent; it is not an omitted assertion.
- Full focused staged-store/reader/maintenance/Clear rerun passed:
  `/tmp/zccache-1964-accounting-store-regressions.log`.
- Still open: production KV recovery scheduling and crash regression, global
  maintenance writer pause, generation replacement race, semantic Dylint and
  required UI/OS/feature gates, docs/review/full gate/PR merge/main rebase.


### #1964 production recovery scheduling (2026-10-22)

- Both Clear and periodic maintenance previously failed the new production
  regression by retaining an abandoned KV namespace indefinitely (RED:
  `/tmp/zccache-1964-production-kv-recovery-red.log`). Both now invoke bounded
  retired-namespace recovery on the blocking lane outside publication exclusion.
  Clear also retries pre-existing staged trash. Successful unlink credits are
  included; incomplete recovery is logged and retained for retry.
- All focused store/maintenance/reader/Clear tests passed, including both new
  recovery entrypoint tests (`/tmp/zccache-1964-production-kv-recovery-green.log`).
- KV process-exit recovery also passed with a fresh same-key replacement intact;
  full KV suite: 30 passed, one existing allocation-heavy test and one explicitly
  parent-invoked child fixture ignored in the outer harness
  (`/tmp/zccache-1964-kv-process-recovery.log`).
- Next: maintenance global writer/cancellation regression, explicit generation
  replacement race, then semantic Dylint enforcement and final PR gates.


### #1964 maintenance cancellation and generation-race evidence (2026-10-22)

- A real expired-generation maintenance pass pauses its recursive sweep after
  destructive commit. The regression verifies the old entry is unreachable and
  the global publication writer is released, then cancels the request. RED
  exposed premature maintenance-permit release
  (`/tmp/zccache-1964-maintenance-cancel-red.log`).
- The blocking worker now owns the permit and returns it with its result.
  Cancellation cannot release it mid-cleanup; normal completion still retains
  it through the WAL acknowledgement. All 94 focused regressions passed
  (`/tmp/zccache-1964-maintenance-cancel-green.log`).
- A separate deterministic race pauses eviction between scan and native-store
  acquisition, replaces the generation under the same key, resumes commit, and
  proves stale eviction leaves the new pointer/payload intact. All nine sweep
  regressions passed, with the explicitly parent-invoked child fixture ignored
  by the outer harness (`/tmp/zccache-1964-replacement-race.log`).
- Primary remaining #1964 work: implement/register semantic deny-level Dylint
  with alias/UFCS/function-item and live-guard coverage, typed capabilities as
  needed, cross-OS/features, docs/review/full gate, PR merge and main rebase.


### #1964 typed publication and Dylint foundations (2026-10-22)

- Added `ban_sweep_under_exclusion`, registered in workspace discovery,
  ci/lint.py formatting and the existing ci.yml Dylint job. Cache-footprint and
  wiring validators pass. No new workflow file. Raw recursive/empty-directory
  deletion is resolved by DefId, confined to explicit shared sweep/link owners
  and existing private compiler/lifecycle owners. Capturing a raw function item
  is forbidden even in an approved owner; direct alias calls resolve normally.
- Raw alias fixture RED: `/tmp/zccache-1964-dylint-owner-red.log`. Ownership-only
  production checks passed Linux/Windows/macOS selections before guard analysis
  was added (`/tmp/zccache-1964-owner-workspace-dylint.log`). Do not mistake that
  earlier run for validation of the current full guard implementation.
- Current guard analysis uses compiler MoveData and MaybeInitializedPlaces on
  promoted MIR, with guard leaves rather than wrapper roots. Copies of MIR omit
  borrow-checker imaginary edges. The wrapper adds enum discriminant refinement
  without enabling rustc's experimental precise-enum-drop flag. Fixtures cover
  partial moves, complete releases, aliased guards and released Result values.
- MIR findings: mir_built is already stolen during late linting. optimized_mir
  loses local metadata and contains SetDiscriminant/Deinit unsupported by the
  borrowck MoveData builder. Promoted MIR works for the sync fixtures, but some
  production async MIR is already consumed. Current fallback fails closed when
  it cannot validate a guard; do not suppress this or claim production coverage.
  Failed development runs are retained in /tmp/zccache-1964-*-guard-workspace.log.
- The generic core WritePermit also means whole-compile admission; it cannot be
  treated as publication critical-section ownership. Added actual core
  PublicationBarrier / PublicationCommitPermit / PublicationDrainPermit facades,
  with private underlying permits and mandatory Duration on all waits. Async
  drain_for is distinct from blocking_write_for; shared ReadPermit stays the
  existing operation lease. Migrated daemon field/construction, maintenance,
  bounded shutdown/flush return contracts and tests. KV namespace gates now use
  the publication facade instead of a unit data RwLock held through put I/O.
- API RED/GREEN: /tmp/zccache-1964-publication-facade-api-{red,green}.log. Runtime:
  95 daemon regressions pass (/tmp/zccache-1964-publication-facade-runtime.log),
  30 KV tests pass, with existing allocation-heavy and parent-invoked child
  fixtures ignored by the outer harness
  (/tmp/zccache-1964-publication-kv-runtime.log).
- 10 current UI fixtures pass, including actual core commit rejection and
  allowed admission/drain/released commit examples
  (/tmp/zccache-1964-real-publication-ui-green.log). Dependency flag discovery
  uses Cargo JSON from a bounded soldr child. SpawnSpec 0.1.26 lacks env_remove;
  helper clear-and-copies inherited environment except IN_SOLDR_PID, preserving
  the explicit toolchain and default wrapper under CI.
- Existing ban_std_pathbuf caught the new retirement-token PathBuf fields; both
  staged and KV tokens now use NormalizedPath. Their runtime regressions pass.

NEXT (still incomplete, do not push/merge #1964):
1. Give native store EXCLUSIVE commits a distinct sealed capability instead of
   the same FileLock used by key publication, read leases and lifecycle roots.
   Artifact staged_lock.rs is the shared daemon/warm owner; its public
   open_store_lock currently returns a raw File. Audit all consumers before
   migrating. Keep retained readers valid and the 2s maintenance deadline.
2. Separate guard effects: short publication/store commit permits forbid slow
   filesystem work; shared/native operation leases and publication drains
   forbid shared trash sweeps but allow their intentional operation I/O. Generic
   core WritePermit admission is NOT a publication commit. Current matcher was
   updated to PublicationCommitPermit, but native FileLock is still over-broad.
3. Confine PublicationBarrier::drain_for acquisition to
   wal::drain_durable_state_for_shutdown and embedded::flush_embedded_state
   (plus test-only callers); otherwise it bypasses the typed commit policy.
   Resolve method/function-item DefIds. The current API example calls drain_for
   as a signature check; adjust that positive fixture when adding owner policy.
4. Classify actual DetachedStagedTrash / DetachedKvNamespace sweep facades,
   including UFCS, function items, erased callbacks, local helper/closure effects.
   Add actual artifact/core dependency UI inputs and accepted after-release cases.
   Do not rely only on primitive std::fs names. Guard leaks/type erasure also need
   consideration so ownership moves are not mistaken for physical release.
5. Resolve consumed MIR without a fail-open path; typed roles should remove
   admission false positives first. All three OS selections must pass with the
   final guard implementation, relevant feature/UI/planted violations included.
6. Tighten raw owner identity to exact impl/type/functions; retain justified
   distinct private staging owners. Permit only fixed-depth name retirement in
   short commit owners, not recursive scans hidden in helpers.
7. Finish docs, clippy/lint, one clud-review invocation, full clean committed gate,
   separate PR merge, verify issue closed, fetch/switch main/rebase origin/main,
   clean tree and delete feature branches. Then #1965–1971 sequentially.


### #1964 drain acquisition boundary (2026-10-22)

- Captured RED for untrusted publication drain acquisition and factory capture
  (`/tmp/zccache-1964-drain-owner-red.log`). A resolved method, alias/UFCS path or
  factory reference now requires the exact WAL/embedded drain owner. Even an
  approved owner may not export the raw factory as a function item.
- Added actual-core negative alias/method/UFCS/factory cases and an included
  auxiliary source with the genuine crate/module/file/function identity proving
  the shutdown owner is accepted. Unrelated same-named methods remain accepted.
  All 12 fixtures passed (`/tmp/zccache-1964-drain-owner-final.log`).
- Important correction: rustc's def_path_str is display-oriented and can omit
  the local crate prefix. Owner matching and the opaque StagingRoot exemption
  now use canonical get_def_path symbols. Do not use pretty paths for identity.
- Current full production probe still fails in guard analysis
  (`/tmp/zccache-1964-typed-publication-workspace.log`), notably consumed async
  MIR and raw FileLock over-classification. No push, gate attestation or PR.
- Native-store migration design: centralize acquisition in artifact staged_lock;
  expose a sealed StagedCommitGuard::acquire_for(artifact_dir, Duration) and
  bounded StagedReadGuard acquisition. Remove the public raw open_store_lock
  escape, migrate daemon publisher/maintenance/trash/materialization, CLI warm,
  snapshot and coordination tests to typed guards. Owned native locks avoid a
  self-referential borrowed guard. Preserve the exact same lock filename,
  reader-path validity, 2s maintenance contention deferral and zero-wait empty
  trash cleanup. All waits must require Duration; no unbounded compatibility
  overload. Then classify commit vs read/drain effects by resolved types.


### #1964 native-store facade migration (2026-10-22)

- Added sealed artifact::StagedCommitGuard with mandatory acquire_for(root,
  Duration). Raw open_store_lock is now private to artifact staged_lock.
  StagedReadGuard now has acquire_for and acquire_if_present_for with mandatory
  Duration; the parameterless acquisition APIs were removed. It is must_use.
- Migrated daemon publication, generation retirement, Clear auxiliary retirement,
  recovery/empty-trash unlink, pooled hit materialization, snapshot export, CLI
  warming and coordination fixtures. Every participant still uses the canonical
  .staged-v2/.store.lock. Exclusive guards own native handles rather than exposing
  a File or constructing a self-referential borrowing guard. Key publication's
  separate per-key operation lock remains distinct.
- Missing-type/method RED retained in
  /tmp/zccache-1964-store-facade-api-red.log; five native coordination tests GREEN
  (/tmp/zccache-1964-store-facade-api-green.log), including zero-wait contention
  and actual exclusive/shared interference.
- 95 daemon regressions passed with the migrated store API
  (/tmp/zccache-1964-store-facade-runtime.log). 14 snapshot tests and the CLI warm
  contention regression passed (/tmp/zccache-1964-store-facade-consumers.log).
  Its production build caught an unused cfg(test)-only root binding, now removed.
- Single-worker embedded real-hit/control-plane regression passed. It now requires
  rustc instead of silently returning when the compiler is missing; the hardened
  run passed (/tmp/zccache-1964-store-facade-control-plane-required.log).
- Dylint recognizes the actual StagedCommitGuard by defining crate, type and
  parent module DefIds. UI dependency flags now include actual artifact and core
  rlibs, produced via bounded soldr Cargo JSON. All 14 fixtures passed, including
  rejection under a native commit, acceptance after release, and retained-reader
  I/O (/tmp/zccache-1964-native-store-ui-green.log).

Remaining enforcement work before #1964 can ship:
- Separate Critical vs Lease/Drain effects: publication/store commits forbid
  bulk work; reader/admission operation permits allow their intended I/O, while
  shared trash sweeps remain forbidden under reader/native/drain exclusion.
- Make ownership inspection structural: Weak and PhantomData do not own their
  generic guard parameters. SharedState's weak materialization slot previously
  caused phantom FileLock ownership when its local target directly exposed the
  raw native guard. The migrated target now contains opaque StagedReadGuard;
  do not reintroduce that false positive when classifying reader leases.
- Permit only immediate fixed-depth name enumeration in the exact bounded
  detach owners returning typed trash. Do not move enumeration ahead of native
  exclusion without proving publisher/replacement safety. Recursive payload
  scans and helper-hidden slow work must still be rejected under commits.
- Classify actual shared sweep facades and transitive helper/closure effects,
  prevent function-pointer and leaked/erased-guard bypasses, and resolve consumed
  MIR without a fail-open fallback. Add async/callback/actual-native fixture
  coverage. Existing failed production probes are development evidence, not a
  green final guard gate.
- Final OS/features/planted violations, docs, clippy, one clud-review invocation,
  clean committed full gate, PR merge, origin/main rebase and branch cleanup are
  still required. Then continue #1965–1971 in order.

### #1964 structural ownership and cleanup effects (2026-10-22)

- Publication reads now return sealed PublicationReadPermit rather than generic
  admission ReadPermit. Migrated explicit daemon reader-lease signatures without
  changing the underlying barrier or lease lifetime.
- Guard classification follows owned fields/captures, ignores Weak/PhantomData
  and raw pointer/signature-only parameters, and distinguishes short commit/data
  guards from reader/native/drain leases. Bulk filesystem work is forbidden under
  short commits; shared sweeps and unresolved indirect calls are forbidden under
  either exclusion role. Ordinary admission permits remain separate.
- Resolved local helper/closure call effects expose cleanup hidden behind a
  helper. A borrowed guard parameter remains caller-owned even after drop(ref).
  RED logs: ownership-effects-red and borrowed-guard-red under /tmp/zccache-1964-*.
- All 18 UI fixtures passed (/tmp/zccache-1964-ownership-boundaries-final.log),
  including non-owning wrappers, helper-hidden cleanup, reader/drain cleanup,
  cleanup after release, and borrowed guard retention.
- Daemon all-target compiler check with test-support passed
  (/tmp/zccache-1964-publication-reader-feature-check.log). Fixed a preexisting
  test-only staged_root import exposed by checking the production library.
- All 95 declared bosn staged-store-regressions passed
  (/tmp/zccache-1964-publication-reader-runtime.log); the ignored process-exit
  child is invoked by its parent fixture, not silently omitted.
- Consumed MIR fallback now rejects relevant effects conservatively instead of
  rejecting an effect-free body merely because it mentions a guard. This remains
  conservative, not a final async lifetime solution. Full production lint probe
  is /tmp/zccache-1964-structural-effects-workspace.log; do not treat UI/runtime
  receipts as proof of complete production enforcement. Remaining enforcement,
  OS/features, review/full gate and merge/main cleanup requirements above apply.
- Production probe exposed an ICE when effect traversal queried MIR for a
  required trait method with no body. Added hir_maybe_body_owned_by guard;
  bodyless dynamic methods conservatively carry UNKNOWN effects. New actual
  dependency abstract_effect fixture diagnoses the call instead of crashing;
  all 19 fixtures pass (/tmp/zccache-1964-abstract-effect-final.log).
  Previous production probe remains RED; reader callback analysis and production
  enforcement are not yet complete.

### #1964 generic cleanup-effect substitution (2026-10-22)

- Local effect summaries now retain concrete generic arguments and substitute
  them into MIR call targets. Closure and function-item callbacks reached through
  generic helpers retain their actual effects. Recursive summaries track both
  defining symbol and arguments, with conservative UNKNOWN at bounded depth.
- RED: generic_callback incorrectly rejected ordinary read I/O under a reader
  permit (/tmp/zccache-1964-generic-effects-red.log). GREEN: that callback passes
  while a sweep through the same generic FnOnce helper is denied. All 21 UI
  fixtures pass (/tmp/zccache-1964-generic-effects-final.log).

### #1964 bounded pointer revalidation (2026-10-22)

- Replaced unbounded pointer read_to_string calls in maintenance with a private
  mandatory-budget reader: fixed 129-byte stack buffer, 128-byte accepted maximum,
  deadline checked between reads, InvalidData for oversized/invalid UTF-8 input.
  Single kernel calls remain non-interruptible. Short legacy pointer names remain
  supported; this change does not impose a new digest-format migration.
- New oversized-pointer regression proved old generation retirement accepted an
  oversized current pointer (/tmp/zccache-1964-pointer-budget-red.log). Corrected
  implementation rejects it and preserves the existing generation. Existing
  legacy cleanup behavior is unchanged.
- All 96 declared bosn staged-store regressions pass
  (/tmp/zccache-1964-pointer-budget-compatible-green.log).

### #1964 explicit operation phases and effect memoization (2026-10-22)

- Spawn materialization returns sealed MaterializationPermit, preserving its
  existing native RwLock/watchdog lifetime. Its data-free whole-copy role permits
  copy I/O but still excludes shared cleanup. Publication/store commits retain
  their stricter role. All four declared spawn-watchdog tests pass, including
  real termination of the 120-second wedged materialization holder
  (/tmp/zccache-1964-materialization-permit-runtime.log).
- Effect traversal memoizes concrete completed summaries; its limit now counts
  active recursion rather than total callees. This removes graph-width false
  UNKNOWN effects. All 21 UI fixtures pass
  (/tmp/zccache-1964-effect-memo-ui.log).
- Reader reacquisition now matches Option ownership explicitly: the existing
  reader moves through unchanged, and only None calls the generic factory.
- Refresh filesystem-space immediately before taking publication exclusion.
  Volume space is not made atomic with foreign writers by that barrier; live
  in-memory access is still refreshed under it. This removes the potentially
  slow resource query from the commit while retaining the near-commit refresh.
- All 96 staged-store regressions pass again
  (/tmp/zccache-1964-callback-phase-runtime.log). Production lint now has exactly
  two diagnostics, both immediate-name enumeration under Clear exclusion
  (/tmp/zccache-1964-callback-phase-workspace.log). This is not a final green gate;
  bounded fixed-depth detach capability/enforcement and remaining acceptance
  requirements still need completion before review/push/merge.

### #1964 bounded immediate-name detach cursor (2026-10-22)

- Clear now enumerates through private ImmediateChildren with mandatory open_for
  Duration, private native iterator and deadline checks before/after open and
  each entry. It never descends into children; its budget includes caller work
  between entries. Entry errors now propagate instead of being flattened away.
- Focused RED: a zero-budget Clear previously detached live names
  (/tmp/zccache-1964-shallow-budget-red.log). GREEN: timeout preserves the live
  pointer; all 97 staged-store regressions pass
  (/tmp/zccache-1964-shallow-budget-green.log).
- Resolved-symbol Dylint confines constructor calls to the two exact Clear
  detach functions/files, disallows factory capture and aliases outside those
  owners, and recognizes the actual cursor source/defining crate/type. Its
  direct one-level read_dir is the sole metadata exception; other effects in
  its helper graph still propagate. No whole helper/body exemption was added.
- Wrong-owner/factory RED and GREEN retained under /tmp/zccache-1964-shallow-owner-*
  using the actual source file. All 22 fixtures pass (shallow-owner-final.log).
- Full production workspace lib/bin Dylint now passes
  (/tmp/zccache-1964-shallow-workspace.log). This is default Linux evidence only;
  pending guard-escape/callback/async coverage, supported OS/features, final docs,
  clippy, review/full gate, PR merge and main rebase remain required.

### #1964 guard escape, callback, and exact cleanup ownership (2026-10-22)

- Added deny-level semantic checks for guard-typed mem::forget (including function
  items), ManuallyDrop/MaybeUninit construction, Box leak/raw conversion,
  transmute, and ownership-erasing unsizing/casts. Ordinary dereference access
  remains permitted; non-owning Weak/PhantomData can still be forgotten.
- Correct RED escape evidence is guard-escapes-red-valid.log: the initial attempt
  failed dependency compilation on rustc's undropped_manually_drops rule, so that
  first log is not policy RED evidence. The fixture explicitly allows that
  unrelated rustc rule to test this lint's leak prevention.
- Foreign combinators propagate concrete local closure effects. Iterator::for_each
  previously hid a typed trash sweep (iterator-sweep-red.log), and now rejects
  it while allowing a read callback under a reader lease.
- Cleanup permission now matches exact free-function identities or the actual
  impl receiver DefId plus method and source file. Impostor::cleanup and a free
  cleanup in staged_plan.rs previously passed (cleanup-owner-red.log); both now
  fail while StagedCompilePlan::cleanup passes in the same fixture.
- All 25 fixtures pass (/tmp/zccache-1964-owner-escape-final.log). Production
  workspace lib/bin Dylint is green with these checks
  (/tmp/zccache-1964-owner-escape-workspace.log). Other pending acceptance gates
  above still apply. New drop_sweep RED fixture is separate uncommitted work.

### #1964 destructor cleanup analysis checkpoint (2026-10-22)

- RED: explicit mem::drop and implicit scope exit hid a local destructor calling
  DetachedKvNamespace::sweep_for under PublicationReadPermit
  (/tmp/zccache-1964-drop-effects-red.log).
- Added local destructor/owned-field drop effects to call summaries and MIR Drop
  terminators, including enum/tuple/array fields. Native guard release itself is
  not bulk cleanup. Skip uninitialized Drop placeholders in pre-elaboration MIR;
  they cannot execute cleanup after a value has already moved out.
- All 26 fixtures pass (/tmp/zccache-1964-drop-effects-final.log): explicit and
  implicit cleanup are denied under the reader, cleanup after release is accepted.
- Production probe is RED (/tmp/zccache-1964-drop-effects-workspace.log), with 25
  diagnostics mostly caused by conservative UNKNOWN destructor effects. This is
  a development checkpoint, not production-gate completion.
- Temporary type tracing was removed after diagnosis. Trace evidence lives in
  /tmp/zccache-1964-drop-types-trace.log. Unresolved types include generic callback
  parameters, generic future/result types, HostAdmissionClassifier trait objects,
  boxed admission/process futures, and dyn Send+Sync. Do not silence UNKNOWN or
  claim a green production run using earlier pre-destructor receipts. Determine
  concrete drop ownership/substitution or supply an enforceable closed contract.
- Additional static cases still need coverage: closure-owned cleanup dropped
  without invoking the closure; async/native sweep execution and consumed-MIR
  behavior; typed transfer/release boundaries. Then complete OS/features/planted
  violations, docs, clippy, review/full gate, PR and main cleanup before #1965.

### #1964 passive/captured drop ownership and closed lease sources (2026-10-22)

- RED: passive_drop treated Weak, PhantomData, ManuallyDrop and MaybeUninit as
  dropping their generic cleanup value (/tmp/zccache-1964-passive-drop-red.log).
  Drop analysis now follows actual fields for types without destructors, treats
  ManuallyDrop as non-dropping, and drops only Weak's allocator rather than T.
  Foreign owning destructors still conservatively include their generic values.
  Owning Arc<Cleanup> remains a denied sweep under a reader permit.
- RED: dropping an uninvoked closure with an owned Cleanup capture bypassed
  analysis (captured-drop-red.log). Drop effects now include closure upvars.
  Held-reader capture cleanup is denied; cleanup after release is accepted.
- All 28 fixtures pass (/tmp/zccache-1964-passive-captured-final.log).
- Production resolver now uses a closed Source enum (state-backed or cfg(test)
  direct acquisition), replacing owned generic factory closures. The existing
  bounded native acquisition owners and payload lifetimes are preserved; there
  is no arbitrary factory destructor while returning a retained reader lease.
  Moved resolution to cached_artifact_resolution.rs to keep source below 1K LOC.
- All 97 staged-store regressions passed after the resolver change
  (/tmp/zccache-1964-closed-lease-runtime.log). Latest production evidence with
  passive fixes is still RED (passive-drop-workspace.log); opaque/generic drop
  resolution and actual provisional-owner cleanup under commits remain pending.
- New private-plan cleanup hook/regression and bosn task are separate uncommitted
  work. Initial broad run failed an existing one-second publication observation
  before reaching the new test. First focused run blocked because the test itself
  accessed a shard still held by paused Clear; cancelled exact bosn job 522 and
  verified its managed container had no test process remaining. The corrected
  regression inspects the map only when it can acquire publication ownership.
  Only its subsequent assertion-based RED should count as bug evidence.

### #1964 provisional-owner destructor deferral (2026-10-22)

- Actual bug: Clear's artifacts.clear dropped the last CachedArtifact owner of
  StagedCompilePlan while retaining publication exclusion. The plan's destructor
  recursively deletes its private output root. Maintenance removal had the same
  last-owner risk in artifacts.remove.
- Added path-scoped PrivatePlanCleanup hook and production Clear regression.
  Valid focused RED is /tmp/zccache-1964-provisional-owner-red-bounded.log:
  assertion "private plan destructor retained publication exclusion" failed in
  0.13s after both workers were resumed/drained. Earlier broad/focused attempts
  described above are not the RED receipt for this fix.
- Clear now moves removed records into an outer-scope retirement Vec; maintenance
  passes an outer Vec by mutable reference into its short removal commit. Those
  owners drop after the publication scope, including early-error/panic unwinding.
  Native store and logical invalidation ordering are unchanged.
- Focused GREEN /tmp/zccache-1964-provisional-owner-focused-green.log proves old
  entry unreachability, publication admission and a real KV publication while
  private cleanup is paused, survival of that new value after Clear, and removal
  of the old private root. Declared bosn task is staged-private-cleanup-test.
- Full staged-store-regressions passes all 98 tests with the new fixture
  (/tmp/zccache-1964-provisional-owner-regressions.log), including the previously
  failed one-second observation. No timing threshold or test concurrency changed.

### #1964 initialized drop-field summaries (2026-10-22)

- RED: transferred_cleanup rejected retain(&mut retired, cleanup) even though the
  cleanup owner moves into an outer collection rather than being destroyed
  (/tmp/zccache-1964-drop-transfer-red.log).
- Effect summaries now use initialized-place dataflow for promoted MIR drops,
  remove imaginary borrow-checking edges, and retain optimized MIR's already
  elaborated drops without feeding optimized statements into MoveData.
- New drop_paths shares field/variant initialization semantics between local
  guard checks and transitive summaries. It excludes moved-out fields and inactive
  enum variants, preserves actual remaining siblings/destructors, and respects
  union/ManuallyDrop non-dropping semantics. This resolves the deferred-owner
  Option tuple placeholders reported at Clear's removal loop.
- All 29 fixtures pass (/tmp/zccache-1964-drop-fields-final.log), including
  Option<(String, Cleanup)> transfer acceptance and partially retained sibling
  cleanup denial. Production diagnostics for Clear and maintenance removal are
  gone, but full production lint remains RED with 26 other diagnostics
  (/tmp/zccache-1964-drop-fields-workspace.log). Do not claim the production gate
  passed based on older pre-destructor logs.
- Remaining work includes consumed async MIR, opaque/generic drop contracts,
  and ownership transfer into checked callees. Any call-boundary exemption must
  retain checks for caller-held sibling guards and generic guard hiding/erasure;
  do not exempt arbitrary moved arguments into unchecked generic helpers.
  Consider preserving promoted MIR before transitive queries consume it, or a
  validated optimized-MIR lifetime backend; neither is implemented yet.
- Native private-plan deferral is already separately GREEN in the 98-test bosn
  regression receipt above. OS/features/planted cases, docs, clippy, one review,
  clean committed full gate, PR merge/main rebase/cleanup remain required.

### #1964 runtime coroutine lifetime analysis checkpoint (2026-10-22)

- Focused RED: an async reader released before dropping a borrowed opaque value
  was rejected solely because promoted MIR was consumed
  (/tmp/zccache-1964-async-snapshot-red.log). Snapshotting at check_crate, including
  reverse body order, proved too late: the async body was already consumed.
  Removed that failed experiment and temporary trace output.
- New runtime_mir works exclusively on a cloned compiler body. It restores
  analysis hints, validates aliases of the real self pointer (all assignments,
  pointer-storage borrows and call results), and models owned frame storage.
  Unknown/ambiguous frame aliases deny compilation; rustc's body is never changed.
- Saved fields use rustc's stable CoroutineSavedLocal identities across variants.
  The lifetime CFG starts unresumed and connects each suspension only to its
  matching continuation. Prefix captures remain owned until actually moved or
  dropped. The first variant-based approach missed a captured reader after yield
  (yield-frame-red.log); that approach was replaced, not exempted.
- Exact guarded move paths are registered via fake reads in an unreachable
  analysis-only block. This avoids collapsing untracked sibling fields into one
  parent. Reader/drain permits remain allowed through async operations; short
  publication/store/data guards are denied when live at Yield/Pending suspension.
- Mutable borrowed captures and nested reference wrappers remain caller-held
  even after drop(reference). Valid RED is async-mut-borrow-red.log (no diagnostic
  before borrowed-capture tracking). async_held_drop diagnoses before release;
  async_released accepts opaque drops after release, including a second await.
  Coroutine fixtures likewise check held and released readers across yields.
- mutated_guard_storage was already denied by the analysis; its initial log is
  an expected-stderr mismatch, not evidence of a new policy bypass. Retained it
  as negative coverage without adding an unsupported mutation rule.
- All 35 compiler fixtures pass
  (/tmp/zccache-1964-runtime-resume-verified.log). New generator fixture uses only
  the pinned nightly compiler harness; no product Rust feature was changed.
- Production probe remains RED with 34 diagnostics
  (/tmp/zccache-1964-resume-lifetime-workspace.log). They are now concrete slow/
  opaque-drop and ownership-transfer sites rather than the old consumed-MIR
  blanket diagnostic. No native alias-preparation ICE or permissive fallback.
- Inspection MIR preserved at /tmp/zccache-soldr-policy-audit/async-released-runtime.mir;
  removed the one-off emitted .mir from the build directory with safe-rm.

Still required before #1964 can land:
- Validate generated coroutine cancellation/drop glue, not just Poll/resume
  execution; current runtime_mir does not traverse coroutine_drop. Check opaque
  captured destructors and ownership ordering during unpolled/pending cancellation.
- Resolve typed ownership transfer into checked local callees/owned blocking
  tasks while retaining checks for caller-held sibling guards. Do not exempt
  arbitrary moved values or unchecked generic helpers.
- Add direct/transitive async sweep lane enforcement with recognized owned
  handoffs. Preserve liveness checks across handoffs rather than treating a
  blocking-task factory as a blanket no-effects boundary.
- Check generic guard erasure/leak helpers and mutable guard storage using
  planted violations before relaxing any call-boundary rule. Existing explicit
  mem::forget/Box erasure cases do not prove generic escape completeness.
- Production green, supported OS/features and planted violations, final docs,
  clippy, one clud-review, clean committed full gate, PR merge/main rebase and
  branch cleanup remain outstanding. Then continue #1965–1971 sequentially.

### #1964 future-capture cancellation checkpoint
- Added `drop_future` compiler fixture: cancelling an unpolled future with a captured recursive-cleanup destructor under publication exclusion must fail; releasing exclusion first must pass.
- Verified RED: `/tmp/zccache-1964-future-drop-red.log` (missing expected diagnostic, exit 101).
- Effects now include coroutine upvar destructors. GREEN: `/tmp/zccache-1964-future-drop-green.log`, all 36 compiler fixtures pass (7 base + 29 examples).
- This covers captured values only, not saved locals created after polling; generated cancellation glue/saved-local coverage remains required. Production lint remains unresolved at checked ownership handoffs/opaque drops. No push or PR yet.

### #1964 saved-local cancellation checkpoint
- Extended `drop_future` with a cleanup constructed inside an async fn and stored across a pending await, plus release-before-drop positive case.
- RED `/tmp/zccache-1964-saved-drop-red.log`: saved-local drop diagnostic absent.
- Coroutine drop summaries now inspect saved field types with instantiated generics. Local opaque return types reveal their concrete hidden drop type; unresolved aliases/foreign coroutine cancellation remain UNKNOWN rather than silently cleanup-free.
- `soldr cargo fmt --manifest-path dylints/ban_sweep_under_exclusion/Cargo.toml --all` passed.
- Current suite `/tmp/zccache-1964-saved-drop-checkpoint.log`: base 7 pass, example 27 pass / 2 fail. `drop_future` passes both required negative diagnostics and release positives. Drain-authorized and drain-wrong-owner fixtures now flag foreign completed acquisition-future drops. Resolve completed-future lifetime/drop semantics or establish a checked facade contract; do not blanket-ignore foreign cleanup. This is an intermediate RED checkpoint, not a passing gate. Production ownership handoff failures also remain.

### #1964 foreign future drop investigation
- Compiler metadata permits inspecting foreign coroutine saved layouts (`is_mir_available`) and opaque hidden types; extended cancellation summaries accordingly and normalize associated aliases through rustc's typing environment. Unavailable metadata stays UNKNOWN.
- `/tmp/zccache-1964-foreign-future-layout.log` and `/tmp/zccache-1964-normalized-future-layout.log` remain RED on the same two drain fixtures, with all cancellation cases accepted/rejected as expected.
- Temporary diagnostic trace `/tmp/zccache-1964-normalized-drop-trace.log` identified concrete UNKNOWN sources: dynamically dispatched Tokio runtime/task callbacks reachable from acquisition-future saved state. Temporary debug output removed. Root issue is all-state drop summaries applied to a coroutine proven completed by await, not absent foreign metadata.
- Next: track Poll::Ready completion for compiler-generated coroutine drop state, retaining conservative pending/cancellation and custom Future destructor behavior. Do not zero all future drops or approve an acquisition-name allowlist. Production owned-transfer diagnostics remain separate work.

### #1964 completed coroutine drop proof
- Added `completed_future.rs`: only a concrete compiler-generated coroutine qualifies. Require resolved Future::poll, unique reference/Pin aliases, uniquely defined Poll result/discriminant, actual Poll::Ready zero edge, and completion on every incoming path. Writes, indirect writes, new mutable borrows, intervening calls, and dropping invalidate completion. Unrelated free `poll` is checked without querying associated-item metadata.
- Guard analysis skips only the direct drop of a coroutine proven completed. Pending/unpolled cancellation remains all-state; custom Future implementations retain field/destructor effects even after Ready.
- Existing two drain fixture failures reproduced in `/tmp/zccache-1964-completed-future.log`; verified GREEN all 36 UI fixtures in `/tmp/zccache-1964-completed-green.log`.
- Added custom Ready Future owning Cleanup to `drop_future`: verified actual diagnostic at await, inspected expected stderr. Added free `poll` call to approved drain positive fixture. Final conservative proof + both boundaries pass all 36 fixtures in `/tmp/zccache-1964-completion-boundaries.log` (terminal 0). Format and diff check pass.
- Production probe `/tmp/zccache-1964-completion-workspace.log` terminal 1, 47 diagnostics, no ICE. This snapshot preceded the final conservative invalidation/free-poll hardening. New saved-local/opaque cancellation checks expose additional drop paths; workspace is not green. Owned handoffs, opaque state drops, transitive completed-future summaries, and cancellation glue inside a future retaining guards still need work; do not treat UI coverage as complete enforcement or a full gate.
- No push/PR. Continue #1964 before later children.

### #1964 instantiated ownership escape prevention
- Planted generic `forget_value<T>` consuming a concrete PublicationReadPermit; prior direct-HIR-only checks missed it. RED `/tmp/zccache-1964-generic-escape-red.log`; GREEN `/tmp/zccache-1964-generic-escape-green.log`.
- Planted generic `erase_value<T: Any>` returning Box<dyn Any>; prior summary missed the instantiated ownership-erasing unsize. RED `/tmp/zccache-1964-generic-erasure-red.log`; GREEN `/tmp/zccache-1964-generic-erasure-green.log`.
- Shared resolved leak predicate between HIR and instantiated effects. ESCAPE effects propagate through local generic helper summaries. MIR casts retain source/destination types and are checked after generic substitution. Direct HIR diagnostics are not duplicated.
- Added positive generic forget/erasure of PhantomData<PublicationReadPermit> while a real permit is held; ownership is not inferred from the marker's type argument.
- Final sequential-after-format validation: `/tmp/zccache-1964-instantiated-escape-verified.log` terminal 0, 7 base + 30 actual-dependency examples pass. Format and git diff checks pass. README now accurately distinguishes current capabilities from remaining #1964 work.
- These changes do not resolve the existing production RED (latest prior probe 47 diagnostics) or establish complete transfer/cancellation/lane enforcement. No push/PR; continue #1964.

### #1964 explicit checked synchronous ownership transfers
- RED `/tmp/zccache-1964-typed-transfer-red.log`: caller rejected a local synchronous helper that explicitly takes PublicationReadPermit and releases it before dropping opaque T. Companion fixture preserves a second caller-held permit and requires rejection.
- Added `checked_transfer.rs`: moves can transfer only to local synchronous functions whose source is covered by the same shared guard-check predicate and whose declared formal type explicitly owns the guard. Generic T, borrowed wrappers, foreign/async callees, and opaque/guard-owning outputs do not qualify. Nested return fields are inspected for unknown ownership.
- Caller excludes only initialized guard move paths inside those exact moved operands. Other owned and borrowed guards remain held. Callee analysis still checks all paths; added wrong-order helper regression that rejects cleanup before its owned permit is released.
- `/tmp/zccache-1964-typed-transfer-boundaries.log` terminal 0: 7 base + 32 actual-dependency examples (39 total) pass, including generic escape tests. soldr cargo fmt --check and git diff --check pass.
- Production `/tmp/zccache-1964-typed-transfer-workspace.log` terminal 1, 44 diagnostics, no ICE (previous snapshot 47). Explicit store_rustc_outputs/store_single_output call diagnostics at miss_store:140/:160 disappear; foreign launch_blocking/captured-state drops and other diagnostics remain. This is not a passing workspace gate, no push/PR.
- Continue checked task ownership/drop ordering, coroutine cancellation with retained guards, async sweep lane, OS/features, review and full gate before completing #1964 or advancing to #1965.

### #1964 checked canonical blocking handoff
- Actual kernal-api 0.1.26 added to compiler-fixture inputs, through Cargo JSON dependency discovery (no stub scheduler).
- RED `/tmp/zccache-1964-blocking-transfer-red.log`: a local closure releasing its permit before opaque cleanup was rejected at canonical launch_blocking. Negative fixtures simultaneously preserve rejection with another caller-held permit and with unsafe cancellation capture order.
- Added exact resolved `kernal_api::async_engine::launch_blocking` contract: local closure body must be covered by guard checking; no borrowed guard captures or opaque/guard-owning outputs; compiler capture drop order must release exclusion before forbidden cleanup. Direct guard/Option/Result/tuple release proofs only. Arc and manually suppressed owners cannot claim last-owner release. Async launch and other foreign calls remain ineligible.
- Invoked body remains independently checked. Wrong-body fixture proves opaque cleanup before permit release still denies inside closure. Arc<Permit> fixture proves shared ownership cannot qualify merely because dropping Arc itself has no cleanup effect.
- Final `/tmp/zccache-1964-blocking-transfer-final.log` terminal 0: 7 base + 34 actual-dependency examples = 41 fixtures pass, including all prior generic leaks/transfers. Formatting and diff check pass. Earlier green attempt failed only rustfmt-shifted negative golden locations; inspected actual diagnostics before updating.
- Production `/tmp/zccache-1964-blocking-transfer-workspace.log` terminal 1: 42 diagnostics, no ICE, versus prior 44. miss_store:421 canonical transfer now accepted; :976 and remaining ownership/drop failures remain. Production snapshot has identical lint implementation to final fixtures; only last Arc negative fixture was added afterward.
- No runtime product source changed this checkpoint; no claim of new runtime regression coverage. No push/PR. Continue #1964 cancellation/ownership/lane and final OS/features/full gate work.

### #1964 publication capture and execution release ordering
- Inspected remaining rejected blocking handoffs: single-output and multi-output publication closures captured InFlightGuard (an Arc<SharedState> owner) before PublicationReadPermit. Cancellation could therefore destroy opaque state ownership while retaining publication exclusion.
- Both callbacks now first borrow publication to establish capture order, then bind lifetime owners, then bind publication last. Cancellation drops the captured publication field first; executed/unwinding callback drops its last-bound publication local before other lifetime owners. Added matching positive compiler fixture to blocking_transfer.
- Source files were already oversized. Extracted depfile sources + identity/index commit helpers from miss_store (now 972 lines) and per-unit cache lookup from handle_compile_multi (now 807). New modules 64/approx50/441 lines. Existing public helper paths preserved by re-exports; index helper parent paths adjusted.
- Declared bosn task publication-miss-regressions. Current runtime evidence `/tmp/zccache-1964-publication-capture-runtime.log`: terminal 0, 8 real publication diagnostics regressions + 11 multi-file tests pass inside managed bosn container. No host runtime tests, isolation spoofing, or threshold edits.
- `/tmp/zccache-1964-capture-binding-ui.log`: terminal 0, all 41 compiler fixtures pass. Workspace format and diff checks pass.
- `/tmp/zccache-1964-capture-order-workspace.log`: terminal 1, 40 diagnostics, no ICE, down from 42. Targeted single/multi blocking launch diagnostics disappear. Async frame/cancellation and opaque-drop failures remain. No push/PR.
- Next source/policy audit also needs private staged-plan cleanup ordering: the Rustc blocking closure still binds its publication local before resource/staged-plan locals, and raw recursive deletion currently shares BULK classification with copy/read work. Verify recursive deletion is forbidden under every publication/store role, while bulk materialization remains allowed under operation/read permits. Do not silently narrow acceptance to shared trash APIs.

### #1964 all recursive deletion is sweep work
- Planted an approved StagedCompilePlan cleanup owner holding PublicationReadPermit during raw remove_dir_all, plus an approved cc owner releasing the permit first. RED `/tmp/zccache-1964-private-tree-red.log`: only impostor diagnostics existed; the approved-but-held deletion was silently accepted.
- Raw std/tokio fs::remove_dir_all now has SWEEP effects, distinct from copy/read bulk effects, independent of approved owner status. Ownership placement and exclusion lifetime are separate checks. GREEN `/tmp/zccache-1964-private-tree-green.log`; final all 41 UI fixtures `/tmp/zccache-1964-private-tree-final-ui.log` terminal 0.
- Stronger production RED `/tmp/zccache-1964-private-plan-source-red.log` terminal 1: 102 diagnostics (previous weaker rule 40). In particular miss_store:440 caught local staging-plan destruction before publication release; other new paths include artifact replacement/map owner destruction under readers. Do not downgrade deletion back to BULK to hide them.
- Rustc blocking publication callback now captures publication first and binds it last, after resource admission and staged plan. All exit/unwind paths release publication before local plan cleanup. miss_store remains below 1000 lines.
- `/tmp/zccache-1964-private-plan-runtime.log` terminal 0: declared bosn publication task passes 8 publication + 11 multi-file runtime regressions. Workspace + Dylint formatting and diff checks pass.
- Final production `/tmp/zccache-1964-private-plan-order-workspace.log` terminal 1, 101 diagnostics, no ICE; the targeted local plan-drop error disappears. This is intentionally still RED and not a workspace gate pass. No push/PR.
- Next: inspect actual private plan/StagingRoot destructor and replacement ownership paths. Existing retirement vectors solve Clear/maintenance only; publication replacements and aggregate/async cancellation lifetimes remain. Consider a checked typed cleanup owner for private-root retirement if it removes recursive work from Drop, but preserve durable retry, cancellation, and no-tree-deletion-under-exclusion contracts; an unchecked background callback/blanket summary exemption is not a fix. Still need async sweep-lane enforcement, supported OS/features, review/full gate and PR merge before #1965.

### #1964 retire replaced Rust cache records outside publication
- Concrete bug: commit_rustc_artifact_index dropped entry.insert's old CachedArtifact under the caller's publication reader. An old provisional entry can own the last private StagedCompilePlan.
- Added real filesystem/hook regression rustc_replacement_releases_publication_before_private_cleanup. Pause last-owner private cleanup; a publication writer must proceed, replacement metadata must already be visible, then resume and verify private root removed. Map lookup is conditional on successful writer admission so old shard-lock behavior cannot hang RED.
- Valid RED `/tmp/zccache-1964-replacement-owner-red-bounded.log`: 8 pass/1 fail in 0.40s, assertion "replacement destructor retained publication exclusion". Earlier `replacement-owner-red.log` was only a private-module test import error and is not valid RED.
- Commit helper now retains displaced owners in a caller-owned Vec. Synchronous Rust publication declares retirement before re-binding publication ownership; detached blocking callback declares its retirement Vec before the last-bound publication permit. Thus default exit/unwind drops exclusion before displaced records. Vacant insertion has no retired owner; merge/commit failure ordering unchanged. All callers migrated.
- GREEN `/tmp/zccache-1964-replacement-retirement-green.log` terminal 0: 9 publication regressions + 11 multi-file tests pass under declared bosn task. New writer/visibility/destructor assertion passes. Intermediate `replacement-owner-green.log` had a mistaken vacant-entry push type error (corrected); it is not a passing receipt.
- Workspace fmt and diff checks pass; miss_store 981 lines, tests 502. Lint implementation/UI fixtures unchanged this checkpoint (prior all41 fixture receipt still applies to that implementation).
- Production `/tmp/zccache-1964-replacement-retirement-verified-workspace.log` terminal 1, 102 diagnostics, no ICE; runtime fix does not make lint green. Helper call at miss_store:434 and new retirement drop at :448 remain flagged, so next inspect exact live move paths/borrowed flags and effect-summary origin rather than suppressing drops. Prior typo-containing production probe is invalid for policy conclusions.
- Checked daemon root lifetime ownership hypothesis: CacheRootWriterLock is a foreign core type with no generic arguments and is already opaque to current guard containment. Do not add a speculative SharedState/root-lock exemption. Private-plan type itself contains only paths/args; trace actual reported guard lifetime before changing classification.
- Remaining #1964 work and #1965–1971 scope unchanged. No push or PR; full gate/review/OS-feature audit still required.

### #1964 ordered closure destruction during unwind
- Exact trace `/tmp/zccache-1964-retirement-guard-trace.log` terminal1 showed miss_store:434 held a real publication local (_6) at the helper call; :448 held the closure's still-unmoved capture (_1.0) on unwind. borrowed=false. Thus the closing-drop diagnostic is not a daemon/root lifecycle-lock classification issue.
- Removed all temporary trace output. Extended positive binding_order fixture with Vec::<u8>::new before captures move into locals; valid RED `/tmp/zccache-1964-ordered-closure-unwind-red.log` terminal101 at closure closing drop. This isolates the unwind path even though capture destruction releases the guard first.
- Reused the existing canonical blocking cancellation release proof for a whole compiler-generated closure drop only when every initialized owned guard is inside that drop place and no borrowed guard remains. Check both Critical and Any capture order. Custom destructors, uncertain wrappers, outside guards and bad capture order retain existing denial. No arbitrary aggregate/drop exemption.
- GREEN `/tmp/zccache-1964-ordered-closure-unwind-green.log` terminal0: all41 compiler fixtures pass, including captured cleanup, partial moves, external guards, unsafe cancellation order and generic escapes. Format/diff checks pass.
- Final production `/tmp/zccache-1964-ordered-closure-workspace.log` terminal1: 101 diagnostics, no ICE; prior102 and miss_store:448 closing-drop diagnostic now absent. Helper call :434 still has a SWEEP summary under a real publication guard; next trace concrete effect origins/initialized drop variants in that helper graph before assuming the runtime-retirement fix removes every potential path.
- No runtime product code changed; d57835a7's valid20-test isolated runtime receipt remains applicable. No push/PR. Goal and remaining #1964 / later child scope remain active.

### #1964 known local enum initialization in cleanup effects
- Temporary summary trace `/tmp/zccache-1964-index-effects-trace.log` terminal1 identified commit_rustc_artifact_index -> CachedArtifact::from_index -> StagedCompilePlan::drop/cleanup -> std::fs::remove_dir_all. from_index explicitly sets private owner None. All temporary SWEEP_ORIGIN trace code removed.
- Planted empty_cleanup_field: a factory initializes cleanup=None before a checked scalar operation that can unwind, returns a holder, and the caller releases its permit before dropping the holder. Valid RED `/tmp/zccache-1964-empty-owner-red.log` terminal101 falsely attributed cleanup to the factory call.
- Added drop_variants: prove only a non-argument place with one explicit ADT aggregate initialization; reject overlapping writes, parent writes, reference/raw-pointer creation, discriminant mutation, call-result writes and inline asm. Structural drop traversal skips only proven inactive variants; custom Drop remains fully analyzed.
- GREEN `/tmp/zccache-1964-empty-owner-green.log`. Added populated-owner and mutation-through-&mut negatives; inspected actual two required diagnostics before recording golden. Final `/tmp/zccache-1964-empty-owner-verified-ui.log` terminal0, 7 base + 36 examples =43 fixtures pass. Formatting and diff checks pass.
- Production `/tmp/zccache-1964-empty-owner-workspace.log` terminal1, still101 diagnostics, no ICE; miss_store:434 helper call remains flagged. Direct local variant proof does not yet handle all nested aggregate fields/transfers in the actual constructor. Next trace exact Drop target types/places within from_index and plant that nested case; do not assume this direct-local fix proves its whole constructor safe or suppress its summary.
- Runtime source unchanged; prior valid20 isolated regression receipt still applies. No push/PR. #1964 and later children remain incomplete.

### #1964 nested known-empty cleanup owners
- Extended positive empty_cleanup_field with a Holder(cleanup=None) fully initialized before checked(value) can unwind, then moved and placed in Arc. RED `/tmp/zccache-1964-nested-empty-red.log` terminal101 falsely marked boxed(value) as a sweep.
- Variant proof now resolves a unique aggregate initializer's field operands and whole-value moves, including tuple/closure fields, through at most32 source steps. Reject args, multiple/overlapping writes, references/raw pointers, discriminant mutation, call-result initialization, unions and unsupported projections. Custom destructors remain opaque effects, not skipped by variant selection.
- GREEN `/tmp/zccache-1964-nested-empty-green.log`; strengthened positive with an actual whole-value move and negative with a nested populated owner. Inspected all3 actual negative diagnostics before updating expected output.
- Final `/tmp/zccache-1964-nested-owner-verified-ui.log` terminal0: all43 compiler fixtures pass. Format/diff checks pass. README documents proof limits.
- Production `/tmp/zccache-1964-nested-owner-workspace.log` terminal1:96 diagnostics, down from101, no ICE. miss_store:434 index-commit call now absent, matching from_index's actual known-None private owner rather than exempting its type/function. Native reader/drop findings elsewhere still apply.
- No runtime product source changed; valid20 isolated tests from replacement retirement remain applicable. No push/PR. Continue remaining #1964 production drop/cancellation/lane paths and all OS/features/full-gate/review requirements before later children.

### #1964 automatic owner field drop ordering and native lease facade
- RED `/tmp/zccache-1964-ordered-struct-red.log` terminal101: a plain Owner declaring PublicationReadPermit before Cleanup was rejected at implicit unwind/return drop even though Rust releases the permit first.
- Generalized the existing capture-order proof into ordered_drop for automatic local structs/enums, tuples and closure captures, plus standard Option/Result. Each variant checked independently; nested owners can order only their own guards. Depth32 fails closed. No custom application Drop or shared/manual-wrapper release proof; other owned/borrowed guards remain live through the caller's all-inside test.
- Inspected actual StagedMaterializationGuard/Lock definition: private facade holds a shared Arc to StagedReadGuard and timing fields. Added exact canonical DefId-path reader role to that facade. This tracks each materialization lease's ownership at its API boundary; it does not assert global last-reader release or exempt generic Arc<Guard>. Existing Arc negative remains required/passing.
- Positive ordered/nested-owner tests and reversed-order, external borrowed guard, custom Drop negatives. Inspected all5 expected diagnostics before recording golden. Final `/tmp/zccache-1964-ordered-owner-verified-ui.log` terminal0, 7 base +38 examples =45 fixtures pass. Format/diff checks pass.
- Production `/tmp/zccache-1964-ordered-owner-workspace.log` terminal1:95 diagnostics (previous96), no ICE. cached_artifact_resolution:98 return-value drop diagnostic disappears with explicit lease role and ordered automatic payload destruction. No blanket resolver exemption.
- Runtime product source unchanged; prior20 isolated runtime regressions still apply. Next address remaining actual replacement/destructor and async frame/lane paths. No push/PR; #1964 and later child scope remains incomplete.

### #1964 retire displaced error-verdict records outside publication
- Found same concrete last-owner bug in commit_rustc_verdict: occupied-entry replacement dropped the old provisional record under the caller's publication reader.
- Added real paused-private-cleanup regression for verdict replacement. Valid RED `/tmp/zccache-1964-verdict-retirement-red.log` terminal101:5 pass/1 fail in0.35s at "replacement destructor retained publication exclusion". Writer/map probes remain bounded and map lookup conditional on writer admission.
- Verdict commit now retains displaced records in caller-owned retirement storage. maybe_store_rustc_error_artifact declares it before publication, explicitly releases publication, then schedules record destruction on a blocking worker.
- New private retire_verdict_artifacts_for requires Duration and schedules synchronously before returning a waiter. Timeout/caller cancellation detaches the already-owned worker; the returned future does not retain cleanup records pending its first poll. Empty work starts no worker; join failure and wait expiry are logged.
- Added unpolled_verdict_cleanup_waiter_keeps_owned_worker: actual private-plan destructor reaches hook before polling the waiter, survives waiter drop, and completes filesystem removal after resume.
- Added declared bosn error-verdict-regressions task. GREEN `/tmp/zccache-1964-verdict-retirement-green.log` terminal0: all7 error-cache tests pass, including existing WAL ordering, metadata/sibling-verdict preservation and Clear-barrier tests. Format/diff checks pass. Source remains below1000 lines.
- Production `/tmp/zccache-1964-verdict-retirement-workspace.log` terminal1:94 diagnostics, previous95, no ICE; error_cache publication call diagnostic absent. Lint implementation unchanged (prior45 fixture receipt applies). No push/PR; remaining #1964 and later child scope unchanged.

### #1964 lookup owner release and explicit language drop
- Exact trace `/tmp/zccache-1964-hit-guard-trace.log` identified publication inside CachedArtifactLookup rather than a borrowed SharedState guard. Temporary trace removed. Lookup declared cached record first, permit last, so default Drop could recursively clean a last private owner before releasing publication.
- Added actual lookup regression: remove the live-map record while a lookup retains the sole cached owner, pause its private-plan destructor, and prove publication writer proceeds before cleanup resumes. Valid RED `/tmp/zccache-1964-lookup-owner-red.log` terminal101:5 pass/1 fail in0.35s at "lookup drop retained publication during private cleanup".
- Reordered lookup fields and initializer: publication permit first, cached owner last. Lifetime during lookup unchanged; normal/unwind destruction releases publication before private cleanup.
- Declared bosn lookup-retirement-regressions. GREEN `/tmp/zccache-1964-lookup-owner-green.log` terminal0, all6 real deferred-load/lookup tests pass.
- Also planted explicit drop(Owner { permit, cleanup }) with safe field order. RED `/tmp/zccache-1964-explicit-owner-drop-red.log` terminal101. Ordered proof previously handled only MIR Drop. It now also handles a moved operand to resolved core::mem::drop; references/dynamic function pointers/custom destructors/outside guards remain ineligible.
- Added alias and function-item positives, explicit reversed-order and borrowed-other-guard negatives. Inspected all9 negative diagnostics including unwind sites before updating golden. Final `/tmp/zccache-1964-explicit-drop-verified-ui.log` terminal0: all45 compiler fixtures pass. Format/diff checks pass; no trace output remains.
- Production `/tmp/zccache-1964-lookup-release-workspace.log` terminal1:90 diagnostics, previous94, no ICE. cached_hit:148/:287/:491 findings disappear. Remaining :313/:335/:654 involve payload resolution, separately held native materialization leases and/or temporary reference paths; trace those rather than exempting the hit routine.
- Prevention audit next should plant raw destruction routes (ptr::drop_in_place, ManuallyDrop::drop, MaybeUninit::assume_init_drop) under a held guard: the newly proven mem::drop transfer does not prove those effect summaries complete. Also retain all cancellation, async sweep-lane, OS/features, review/full-gate requirements before closing #1964. No push/PR; all later child scope remains active.

### #1964 raw destructor effect prevention
- Planted actual Cleanup destructor invoked under PublicationReadPermit through std::ptr::drop_in_place, ManuallyDrop::drop and MaybeUninit::assume_init_drop. Valid RED `/tmp/zccache-1964-raw-destructor-red.log` terminal101: all3 expected diagnostics absent, so these resolved standard APIs bypassed cleanup effects.
- Extended core/std destructor symbol classification to evaluate target T's Drop effects for those APIs. This is effect propagation only: raw pointers/borrowed slots never get the moved-owner release proof used by language mem::drop.
- Added resolved import-alias and UFCS method function-item negatives, and release-before-drop positives for all3 APIs. Inspected all5 negative diagnostics before recording final golden. Early green attempt failed underline formatting only, not missing policy diagnostics.
- Final `/tmp/zccache-1964-raw-destructor-verified-ui.log` terminal0:7 base +39 examples =46 fixtures pass. Format/diff checks pass.
- Production `/tmp/zccache-1964-raw-destructor-workspace.log` terminal1:still90 diagnostics, no ICE. This closes prevention bypasses without suppressing existing violations. Runtime source unchanged; earlier real runtime receipts remain applicable.
- Remaining prevention audit must still cover owned guard/operation-capability erasure through other raw memory APIs (e.g. ptr read/write/copy, unsafe vector length changes), supported owned/mapped guard variants, coroutine cancellation and async sweep-lane enforcement. Investigate with planted cases before changing rules; do not claim the raw destructor cases prove arbitrary unsafe code safe.
- Remaining cache-hit actual lease/owner lifetimes and production90 diagnostics, OS/features, review/full gate, PR merge/rebase and later children all still required. No push/PR.

### #1964 raw guard-memory prevention
- Planted real PublicationReadPermit pointer write/read/copy and Vec::set_len cases, with byte-operation positive controls. Baseline compiler run accepted all cases; required-diagnostic RED `/tmp/zccache-1964-raw-memory-red-expected.log` terminal101 confirms missing enforcement (empty actual stderr).
- Extended resolved standard-library symbol checks to instantiated pointer read/write/copy variants and Vec::set_len when their element type owns exclusion. Raw pointers do not receive an owned-release proof. This rejects unsafe ownership tracking bypasses without banning ordinary byte memory operations.
- Inspected all5 actual diagnostics covering imported alias, function item and UFCS before recording expected stderr. DYLINT_TEST_BLESS was unsupported; that intermediate run failed expected-output comparison only. Verified GREEN `/tmp/zccache-1964-raw-memory-green.log` terminal0:7 base +40 examples =47 fixtures. Formatting and diff checks pass.
- Workspace validation `/tmp/zccache-1964-raw-memory-workspace.log` running at this checkpoint; no claim of production success. Remaining production ownership fixes, cancellation/lane/watchdog audits, OS/features, review/full gate and PR remain required. No push/PR.
- Final workspace probe terminal1: daemon-core reports90 previous errors, unchanged from the preceding probe; no new guard-erasure diagnostics or compiler ICE. `/tmp/zccache-1964-raw-memory-workspace.log` is authoritative for this revision. No active probe remains.

### #1964 generic-tool replacement retirement
- Production handle_exec inserted a replacement CachedArtifact while retaining staged publication. A displaced last private plan owner could therefore recurse through filesystem cleanup before releasing publication.
- Extracted the existing final insertion/release into exec_publication without changing its initial ordering, and planted a real-map/private-plan pause regression. Valid RED `/tmp/zccache-1964-exec-retirement-red.log` terminal101:2 existing staged exec salvage tests passed; new test failed in0.26s at "exec replacement retained publication during private cleanup".
- The final helper accepts a definite replacement, declares retirement storage before the permit, retains the actual insert result, releases publication, then schedules owned blocking cleanup before returning a bounded waiter. The caller only invokes it for a pending replacement. Delivery failures still return before insertion. Normal, unpolled-waiter and cancellation cleanup ownership use the same already-tested mechanism as verdict retirement.
- Moved the unchanged verdict retirement mechanism to cached_artifact_retirement; both callers share it. No global guard/destructor exemptions were added.
- Initial GREEN `/tmp/zccache-1964-exec-retirement-green.log` terminal0:3 exec-publication matches +7 verdict tests. Expanded integration `/tmp/zccache-1964-exec-retirement-verified.log` terminal0:1 new regression +20 complete handle_exec tests +7 verdict tests. The intermediate workspace probe still reported90 diagnostics, including an optional-input scope-exit diagnostic in the new helper. Refined the helper to a definite record with an uninspected insert result until publication release.
- Final isolated task `/tmp/zccache-1964-exec-retirement-final.log` completed successfully with the refined helper:1 +20 +7 =28 tests; actual private cleanup completes despite an unpolled dropped waiter, replacement is visible during paused cleanup, and a publication writer proceeds. Final workspace probe still running at this checkpoint.
- Final workspace `/tmp/zccache-1964-exec-retirement-final-workspace.log` terminal1:90 daemon-core diagnostics, no ICE. The exec_publication helper finding and former bare-insert finding are absent; surrounding exec async-scope/cancellation findings remain. This runtime fix does not claim those are resolved. Source files remain below1000 lines, formatting/diff checks pass. No active probe remains. No push/PR; remaining #1964 acceptance and later children unchanged.

### #1964 borrow delivery from the owned cached record
- Cached payload leases independently cloned a private cleanup owner, allowing its destructor to be summarized as recursive cleanup under separately live publication/native leases. Introduced a real-type compiler bridge behind test-support: its public fixture wrapper contains the actual private MaterializationPayloads, with no synthetic lifetime marker or alternative lease algorithm.
- Valid RED `/tmp/zccache-1964-payload-borrow-red.log` terminal101:all47 existing fixtures and the positive owner/lease control pass, but the early-owner-drop case compiles with empty stderr instead of required E0505. Actual API access is proven, so this is not a privacy/import failure.
- MaterializationPayloads now borrows the owned CachedArtifact clone. An empty Drop implementation retains the borrow through native field destruction; it performs no I/O. Provisional status reads the borrowed record; no private-plan owner clone is created by resolution. Existing private-file runtime regression retains its owned hit until delivery ends, verifies bytes remain live after original ownership is removed, then proves cleanup after the hit drops.
- CachedArtifactLookup::into_parts returns publication first/record last, preserving safe discarded-tuple field order. Compile-hit code rebinds publication last, releases it at the same pre-copy point, drops payload/native lease after delivery and then the cached record. Error and unwind order release publication before record cleanup. Other consumers continue borrowing an owned clone, with no DashMap guard retained.
- Added real compiler contracts for both access after owner-drop and owner-drop after the last access but before lease destruction. Inspected E0505 diagnostics (including destructor use) before recording golden outputs. Initial GREEN/drop-contract runs failed only golden formatting. Final `/tmp/zccache-1964-payload-borrow-verified-ui.log` terminal0:7 base +40 examples +3 native contracts =50 fixtures pass.
- Isolated consumers `/tmp/zccache-1964-payload-borrow-consumers.log` terminal0:5 link +20 exec +11 multi +17 cached-hit =53 tests, including existing materialization budgets and output behavior.
- Initial staged runtime `/tmp/zccache-1964-payload-borrow-staged.log` terminal101:37 staged tests passed;10 cached tests passed and one source assertion still inspected handle_compile_multi.rs after the prior lookup split. Corrected that assertion to inspect the actual handle_compile_multi_lookup.rs owner. No runtime expectation was removed.
- Next staged run `/tmp/zccache-1964-payload-borrow-staged-final.log` terminal101:35 pass/2 fail at the original one-second paused-sweep publisher completion check. Added an independent zero-wait native-read probe and publication-start diagnostic, retaining the original one-second deadline and all full-publication/detach assertions. Final `/tmp/zccache-1964-payload-borrow-staged-diagnostic.log` terminal0:37 staged +11 cached +39 maintenance +11 Clear =98 pass; native lock probes pass. The earlier timing failures are retained; their cause is not established by a later passing run.
- Production `/tmp/zccache-1964-payload-borrow-workspace.log` terminal1:77 diagnostics, down from90, no ICE. Resolver and direct payload/drop call diagnostics disappear; cached-hit end-of-scope and other ownership/cancellation findings remain. No lint suppression or effect downgrade. Docs, format and diff checks pass; files stay under1000 lines. No active probes remain. No push/PR; all remaining #1964 acceptance, gate/OS/features/review and later children still required.

### #1964 eliminate execution-impossible drop unwinds
- Temporary trace `/tmp/zccache-1964-hit-final-scope-trace.log`, predecessor trace and full-body trace identified native publication/payload values at a cached-record cleanup join. The pre-elaboration CFG contains drops of moved temporaries; skipping diagnostics at those drops does not remove their impossible unwind edges from subsequent guard initialization. All temporary trace code was removed.
- Added small ordered-local compiler positives with tuple destructuring/rebinding and a release branch. Valid RED `/tmp/zccache-1964-ordered-local-unwind-red.log` terminal101:both positives incorrectly report recursive cleanup at scope exit despite publication being declared last/released first.
- Added dead_drop_edges: on the lint's existing private MIR clone, use compiler move initialization and structural drop leaves to replace a Drop with its normal successor only if no initialized value needs destruction. Untracked storage remains initialized conservatively. Iterate until no further dead drop is proved; each pass removes a Drop. Live custom destructors retain unwind edges and original compiler MIR is untouched.
- First GREEN attempt `/tmp/zccache-1964-ordered-local-unwind-green.log` confirms both new positives pass. One existing golden changes from9 to6 diagnostics:three duplicate moved-value/unwind sites disappear, while every reversed-order/borrowed/custom-Drop violation remains diagnosed. Inspected all6 surviving diagnostics before updating the golden.
- Negative companion moves one field out of a tuple while retaining a Panics destructor; its real unwind into cleanup while publication lives remains rejected. An initial extra Try-helper fixture crossed the separately documented unsupported guard-owning-return transfer boundary and was not valid evidence for this fix. Replaced it with an owned Result input positive, exercising native `?` lowering without that helper contract.
- Evaluated additional enum drop-path registration, then removed it:the owned-Result control passes with existing variant tracking. `/tmp/zccache-1964-dead-drop-enum-red.log` is actually terminal0, not RED evidence. No speculative enum-path changes remain.
- Final `/tmp/zccache-1964-dead-drop-verified-ui.log` terminal0:7 base +42 examples +3 owner contracts =52 fixtures pass. All existing raw destruction, guard escape, cancellation, generic and ordered-owner negatives remain covered. Runtime source unchanged; no runtime test is claimed for this compiler-only change.
- Initial full workspace `/tmp/zccache-1964-dead-drop-workspace.log` terminal1:76 diagnostics, down from77, no ICE. Exact comparison removes store_outcome.rs:687:14 only, adds none; cached-hit scope-exit still remains and must not be declared fixed. Final full workspace probe running at this checkpoint. Formatting/diff checks pass; no trace code remains. No push/PR; remaining #1964 prevention/ownership/platform/gate work and all later children unchanged.
- Final `/tmp/zccache-1964-dead-drop-final-workspace.log` terminal1 confirms76 production diagnostics, no ICE. No active probe remains. New compiler source is under1000 lines. No push/PR.

### #1964 multi-source replacement retirement
- Found another actual last-owner cleanup path: the blocking multi-source miss worker discarded DashMap::insert's old CachedArtifact while retaining its publication reader through metadata/WAL bookkeeping.
- Extracted the same insertion into multi_publication::replace_cached_artifact with an initially unused retirement sink, preserving the old immediate destruction for RED. Added a real map/private-plan pause regression. Valid `/tmp/zccache-1964-multi-retirement-red.log` terminal101:0 pass/1 fail in0.19s at "multi replacement retained publication during private cleanup". Workers are resumed/drained before the assertion, so the baseline terminates.
- The helper now distinguishes occupied/vacant entries and moves the displaced record into the caller's sink. The actual blocking worker declares the sink before publication; normal return and unwind release publication before the sink's private cleanup. Existing metadata, stats and WAL ordering remain inside publication. No async-worker cleanup handoff is required for this already-blocking worker.
- GREEN `/tmp/zccache-1964-multi-retirement-green.log` terminal0:the new regression exercises both normal completion and an actual worker panic, proving publication writer progress, replacement visibility during paused cleanup, expected join result and eventual private-root removal. All11 existing compile_multi tests also pass (12 tests total).
- Production `/tmp/zccache-1964-multi-retirement-workspace.log` terminal1:still76 diagnostics, no ICE. The helper call and surrounding async scopes remain diagnosed; this runtime repair does not claim a complete static ownership proof. Inspected the pinned DashMap6.2.1 ownership layout: its native lock aliases are already lock_api guards; no speculative guard-role changes were made. Future analysis should trace the actual helper summary/argument and unwind effects instead of broadly exempting map entries or cached owners.
- Formatting and diff checks pass. New source109 lines; multi handler813 lines. No active probe remains. No push/PR; remaining #1964 enforcement/ownership/platform/full-gate/review requirements and later children are still active.

### #1964 narrow multi publication to durable metadata
- Traced the retained helper-call effect in `/tmp/zccache-1964-multi-helper-effects.log`:the effect list includes destruction of the incoming CachedArtifact's Arc<Inner> on unwind before insertion. The prior helper accepted arbitrary provisional cleanup ownership even though the multi-source path inserts durable metadata. Temporary trace code was removed; effects.rs is unchanged from HEAD.
- Changed replace_cached_artifact to accept ArtifactIndex. Entry acquisition happens before constructing a fresh durable CachedArtifact directly at the occupied/vacant insertion. The caller passes its metadata clone and no longer constructs an arbitrary cached owner before key cloning/lookup. This excludes the incoming provisional-owner hazard by type and preserves the retirement sink and metadata/WAL ordering.
- Isolated `/tmp/zccache-1964-multi-durable-interface-green.log` terminal0:the real paused private cleanup regression still covers normal/unwind writer progress, replacement visibility and cleanup, and all11 existing compile_multi tests pass (12 total).
- Full workspace `/tmp/zccache-1964-multi-durable-interface-workspace.log` terminal1:74 diagnostics, down from76. The multi replacement call and local publication-scope findings are absent; multi async handoff/frame findings remain. No ICE, lint exception, effect downgrade or static rule change.
- Format/diff checks pass. No active probes remain. No push/PR; #1964 still needs the remaining ownership/cancellation/lane/watchdog/platform/gate/review work, followed by all later children and required merge/rebase cleanup.

### #1964 direct exec retirement, root job authority and bounded admission
- Direct exec discarded the prior CachedArtifact on its async caller while publication still needed to reach the persistence task. Added a real store_exec_artifact/private-plan pause regression. Valid RED `/tmp/zccache-1964-direct-exec-retirement-red.log` terminal101:existing exec replacement passes; new test fails in4.13s at "direct exec kept its async caller in private cleanup". Hook is resumed and workers drained before asserting.
- Transfer the original publication lease directly into persistence, then synchronously schedule displaced-record cleanup on an owned blocking worker before returning an async waiter. No publication reacquisition or post-insertion caller suspension was added. Queue an async observer containing only the blocking task/budget. Skip the observer when no displaced record exists.
- An initial Arc<SharedState> retention prototype passed29 focused tests, but stronger native evidence disproved its root-ownership claim: `/tmp/zccache-1964-direct-exec-root-lease-red.log` terminal101 reports "explicit shutdown release bypassed active cleanup ownership". CacheRootWriterLock::release explicitly unlocks even while SharedState arcs survive. The prototype is not the final implementation.
- Added core CacheRootWriterLease and mandatory-duration lease_for. Server release closes admission and drops its native claim; existing job leases retain the actual OwnedFileLock through completion. Final native handle destruction runs outside the controller's data mutex. Jobs retain a narrow lease without retaining the full cache state. Pinned kernal-api0.1.26 represents OwnedFileLock as Option<File> on the common implementation; no speculative extra mutex or platform exception was added.
- Both direct persistence and retirement own root leases. Closure cancellation/declaration order releases publication first and retains root ownership through record destruction. The strengthened regression explicitly releases server ownership and drops the server during paused cleanup, proves a second native writer is refused, proves SharedState is released, then proves root reuse after cleanup. Root writer tests also cover two leases, closed lease admission, controller Drop, idempotent release and existing snapshot/restart contracts.
- Extracted exec persistence into exec_persistence.rs to keep the handler under1000 lines; public/internal wrapper paths and existing queued-Clear handoff behavior remain tested.
- Captured ownership-enforcement RED `/tmp/zccache-1964-root-capability-red-controls.log` terminal101:all43 other example cases including valid root work pass; the real root-lease leak fixture compiles with empty stderr. Earlier red.log had an unrelated Copy-forget warning in its positive control; switched that control to a generic helper and reran before implementation.
- Added a separate ownership role for the exact core lease DefId. It participates in resolved leak/erasure checks and instantiated helper effects without becoming publication/store exclusion. Reviewed all6 final diagnostics:import-alias forget, generic forget, direct/generic trait erasure, ManuallyDrop and raw overwrite. Positive controls retain cleanup/async-operation lifetimes and non-owning markers. Blocking handoff cancellation still checks exclusion order; ownership-only leases remain live through cleanup. No blanket effect suppression or async-owner exception was added. Preserved the preexisting opaque staging-root boundary for the new role after an initial full probe took292s through unintended ownership traversal.
- Final compiler `/tmp/zccache-1964-root-jobs-verified-ui.log` terminal0:7 base +44 examples +3 real lifetime contracts =54 fixtures. Intermediate runs failed golden formatting only; all6 actual root diagnostics were inspected before updating expected stderr.
- Added a mandatory admission_bound to the private persistence entrypoint, with30s operation policy at the ordinary wrapper. Valid RED `/tmp/zccache-1964-direct-exec-admission-red.log` terminal101:20 existing handler tests pass; unavailable capacity retains publication beyond the supplied5ms budget. GREEN wraps acquisition in that budget, warns on expiry/closure, and settles the handoff while releasing publication/root authority. It preserves async semaphore admission before the blocking job, avoiding a new blocking-pool wait cycle.
- Final isolated `/tmp/zccache-1964-direct-exec-admission-green.log` terminal0:2 publication/retirement +21 exec +7 verdict +8 root-writer =38 tests. Earlier native-root green and final-runtime receipts also pass; no kernel syscall termination is claimed by these acquisition/wait budgets.
- Targeted Clippy initially found the earlier maintenance retirement sink had raised remove_planned_artifacts to8 arguments. Grouped scanned entries and their selected plan into RemovalSelection, preserving both call paths and retirement scope. `/tmp/zccache-1964-maintenance-selection-green.log` terminal0:all39 maintenance tests. Final targeted all-target Clippy for core/daemon-core with test-support and -D warnings passes `/tmp/zccache-1964-root-leases-clippy-final.log`.
- Final full workspace `/tmp/zccache-1964-root-jobs-final-workspace.log` terminal1:74 diagnostics, no ICE. This is the current final source, including the maintenance input grouping. Earlier73/75 probes reflect intermediate variants and are not final success evidence. Format/diff checks pass, no trace/stub code remains, and source files stay below1000 lines. No active probe remains. No push/PR.
- Remaining audits before #1964 can land:other Clear/maintenance/post-publication workers must be checked against explicit root release (retaining Arc<SharedState> alone is insufficient); remaining scope/cancellation/async-lane findings; new short commit holder watchdogs; owned/mapped guard identities; opaque/generic capability factories and unresolved callbacks; legacy unlink work/classification inside maintenance publication. Plant reproductions before changing proof rules. All OS/features, review and full isolated gate still required, followed by later children and mandatory PR merge/rebase cleanup.

### #1964 — cancelled Clear/maintenance native-root ownership

- Resumed after the issue-only kernel task (kernal-api#433); the previous turn verified that external issue, without changing this repo. Current implementation work remains #1964, sequentially after the three merged children.
- Strengthened both existing paused-sweep cancellation regressions: abort the actual Clear/maintenance requester, explicitly release the daemon's cache-root controller, and probe the actual native writer lock from the blocking lane. Keep publication-release and maintenance-permit checks, resume/drain before asserting.
- RED `/tmp/zccache-1964-cancelled-root-red.log` terminal101: both strengthened tests fail because another native writer is admitted during paused cleanup; the other nine Clear tests and the preceding staged/cached/maintenance suites pass.
- Clear and maintenance acquire a bounded root lease before launching each accepted cleanup worker. Queued closure capture order and execution locals retain that lease through the worker state handle drop. Released controllers reject new work. Maintenance also retains a separate async completion lease through the full-pass marker.
- GREEN `/tmp/zccache-1964-cancelled-root-green.log` terminal0:37 staged +11 cached +39 maintenance +11 Clear =98 pass (one preexisting ignored staged test remains ignored). Both new native-lock assertions pass after explicit release and requester cancellation.
- Targeted all-target Clippy core+daemon-core with test-support and -D warnings passes `/tmp/zccache-1964-cancelled-root-clippy.log` terminal0. Normal soldr wrapper remains enabled in bosn runtime tests; host Clippy uses the previously documented wrapper diagnostic escape.
- Remaining root-ownership work is explicit: WAL Clear/Remove/Flush commands call ArtifactStore::flush_async, whose detached blocking job does not currently carry root ownership. A worker/request lease does not prove safety after acknowledgement timeout/cancellation. The actor and actual disk-write ownership need their own regression/fix; do not claim this worker fix closes that audit.
- SharedState::launch_blocking adds an outer activity owner; its eventual last-state destruction also needs ownership review separately from this worker-local cleanup proof. No claim is made that a worker-local lease alone covers every other Arc holder or actor.
- Final workspace Dylint `/tmp/zccache-1964-cancelled-root-workspace.log` terminal1:74 remaining slow-work/exclusion diagnostics, no ICE. No suppression or effect downgrade; this change does not claim to fix those findings. Diff/format checks pass, edited source remains below1000 lines, and all test/lint handles are terminal. No push/PR. Next concrete audit is real WAL disk-write ownership under cancelled acknowledgement, including the outer activity-owner lifetime.

### #1964 — WAL disk jobs retain native root ownership

- Previous goal turn made concrete progress in2927f5b2 (cancelled sweep workers); resumed on that clean feature branch. No subtree/worktree or external source checkout.
- Added a store-allocation-scoped test hook immediately inside the actual WAL blocking disk job. Independent stores cannot consume each other's pause hook. The regression drives the real actor, expires Clear's5ms acknowledgement, cancels the actor while its disk write is paused, explicitly releases daemon ownership, and attempts the actual native root lock. It resumes the worker before assertions, observes the actual index rename, then proves root reuse within5s.
- Initial `/tmp/zccache-1964-wal-root-red.log` was a compile failure (missing io import/private module path), not behavioral RED. Corrected the test to use the public internal re-export and actual configured index path. Valid RED `/tmp/zccache-1964-wal-root-valid-red.log` terminal101:3 existing WAL tests pass; the new test fails specifically because the detached write loses native root ownership.
- Actor startup now requires CacheRootWriterLease checked out before spawning, in both standalone and embedded service paths and real test harnesses. All actor disk writes use launch_index_flush, a nonblocking constructor requiring Arc<CacheRootWriterLease>; each detached blocking job retains its own reference through store destruction. Actor cancellation/ack timeout cannot release the claim early. No optional ownership bypass or new waiting overload was added. The service retains its existing explicit shutdown and bounded drain/join policy and sequential FIFO processing.
- Split existing1073-line run.rs before editing startup: artifact hydration moves to run_artifact_loader.rs with the existing re-export, accept-watchdog tests move to run_accept_tests.rs. No behavior or assertion removed; final run.rs994lines.
- Strengthened prior cancelled Clear/maintenance sweep tests to stop/join the WAL actor before explicit release and native probing. This prevents the actor's new service lease from masking the independent worker requirement.
- First native-job GREEN `/tmp/zccache-1964-wal-root-green.log` terminal0:4 WAL tests. Refactored the async joining helper into the nonblocking job constructor, preserving owned cancellation.
- Final runtime `/tmp/zccache-1964-wal-root-staged-green.log` terminal0:37 staged +11 cached +39 maintenance +11 Clear =98 pass, one preexisting staged test ignored. `/tmp/zccache-1964-wal-root-lifecycle-green.log` terminal0:4 WAL +2 accept +1 memory-controller +8 embedded flush/shutdown +2 actual warm restarts +1 staged multi/Clear ordering =18 pass. Declared bosn tasks, normal soldr wrapper, real managed container.
- Targeted daemon-core all-target Clippy with test-support and -D warnings passes `/tmp/zccache-1964-wal-root-clippy.log` terminal0. Workspace Dylint `/tmp/zccache-1964-wal-root-workspace.log` terminal1:74 remaining diagnostics, no ICE; no suppression or effect downgrade.
- Compiler fixture coverage extended for direct/generic leakage and trait erasure of shared root leases, plus valid shared cleanup/blocking job and non-owning Weak controls. Validation pending below.
- Outstanding audits include the outer activity owner's final SharedState destruction, shutdown final-store/checkpoint jobs outside the actor, remaining exclusion/async-lane findings, short commit holder watchdogs, legacy unlink phase/classification, opaque/generic capabilities, owned/mapped guards and full OS/feature/gate/review requirements. No push/PR; #1964 remains open.
- Shared-root UI first run `/tmp/zccache-1964-wal-root-ui.log` terminal101:7 base/43 other examples pass, including all shared cleanup and Weak controls; the negative differs only by the3 expected new resolved ownership diagnostics (direct/generic Arc forget and Arc trait erasure). Inspected all9 actual diagnostics and preserved the prior6 exactly before updating the golden output. No lint implementation change was needed.
- Verified `/tmp/zccache-1964-wal-root-verified-ui.log` terminal0, then fixed the one new positive fixture signature's formatting. Final `/tmp/zccache-1964-wal-root-final-ui.log` terminal0:7 base +44 examples +3 real materialization contracts =54 pass on final source. Workspace and Dylint formatting/diff checks pass; no edited source exceeds1000lines. All runtime/compiler/Clippy handles are terminal. No push or PR.
- Next concrete root-lifetime reproduction: retain a sole private-plan owner in SharedState, arrange final Arc destruction in SharedState::launch_blocking's outer activity owner after the operation returns, pause actual private-plan cleanup, explicitly release daemon ownership, and probe the native lock. A lease bound only inside the operation cannot establish ownership through that outer destructor; the facade must retain root ownership through its activity owner too. Shutdown final-store/checkpoint jobs outside the WAL actor remain a separate actual-IO audit.

### #1964 — root ownership through final activity destruction

- Previous goal turn progressed in4a3a3bc2 (WAL native ownership,116 runtime and54 compiler cases); resumed on its clean feature branch. No worktree/subtree/dependent checkout.
- New regression puts the sole private-plan record in actual SharedState, launches a tracked blocking callback, explicitly releases root ownership, drops the server and permits the callback to return. It pauses the actual private-plan destructor in the wrapper's last activity owner, verifies no spare strong state owner masks destruction, probes the native root lock, resumes/drains, proves the private tree gone and verifies native root reuse after the job joins.
- Valid RED `/tmp/zccache-1964-activity-root-red.log` terminal101: existing Clear/private-plan test passes; new regression fails specifically because callback-local root ownership ends before the outer activity cleanup.
- Added mandatory-root launch_blocking_with_root with a shared private owned wrapper. Closure capture order drops the callback/activity before the owner on queued destruction; local declaration order retains the owner through normal/unwind activity destruction. Clear now hands its checked-out root lease to this outer facade instead of binding it only in the inner operation. Existing root-free tracked launches use the same activity/profile machinery with unit ownership; they are not asserted safe by this change.
- Initial GREEN `/tmp/zccache-1964-activity-root-green.log` terminal0:both focused tests pass. Extended the real last-state regression to injected callback panic; it verifies the expected join failure while retaining the same native-exclusion, private-cleanup and root-reuse assertions.
- Final isolated runtime `/tmp/zccache-1964-activity-root-staged-green.log` terminal0:37 staged +11 cached +39 maintenance +13 Clear/private-plan =100 pass, one preexisting staged test remains ignored. Both normal-return and unwind cases pass. Normal soldr wrapper, declared bosn task, actual managed container.
- Initial targeted all-target daemon-core Clippy passes `/tmp/zccache-1964-activity-root-clippy.log` terminal0. Final rerun after adding the unwind case recorded below.
- First workspace Dylint `/tmp/zccache-1964-activity-root-workspace.log` terminal1 was a preflight refusal while host Clippy held the target build lock, not lint evidence. Waited for that exact Clippy handle to finish, then reran. Actual final `/tmp/zccache-1964-activity-root-final-workspace.log` terminal1:74 remaining diagnostics, no ICE. No lint suppression, effects downgrade or new trusted path.
- Native-root audit remains incomplete: migrate/remove the legacy root-free SharedState launch entrypoint (exec, link, multi and compile-hit consumers plus test wrappers), preserving explicit shutdown failure contracts rather than falling through to uncached work; split the existing oversized link handler before editing. Root lifetime of resident private owners outside tracked jobs and final shutdown/checkpoint writes also needs explicit review. Mandatory root ownership belongs in the typed launch API, not an optional best-effort check. Remaining #1964 exclusion/watchdog/legacy-unlink/async-lane/OS-feature/review/full-gate work still applies. No push/PR.
- Final all-target Clippy with test-support and -D warnings passes `/tmp/zccache-1964-activity-root-final-clippy.log` terminal0. Format/diff checks pass, edited source stays under1000lines and every test/lint handle is terminal. The new facade is verified; migration of the legacy unrooted entrypoint is the next concrete source change.

### Draft handoff — goal paused at user request

- User requested stopping the goal, publishing a draft PR and updating #1964 with current status. Goal set paused; no implementation work is to continue as part of this handoff.
- Preserving the current migration as WIP: removed the root-free tracked launch entrypoint and generic/unit-owner helper; all SharedState tracked launches now require a concrete CacheRootWriterLease. Migrated exec, compile-hit, multi and link consumers and test wrappers. Root release produces terminal Response::Error, not a cache miss; compile-hit callers avoid hit-cache updates for terminal errors. Split the preexisting oversized link handler and compile pipeline before editing.
- Focused valid RED `/tmp/zccache-1964-root-launch-red.log` terminal101:2 existing released-root tests pass; new actual exec-hit test fails because released ownership still permits materialization/a nonterminal outcome.
- Focused GREEN `/tmp/zccache-1964-root-launch-green.log` terminal0:3 released-root tests pass, including rejected actual exec materialization and zero active requests. This compiles the current migration, but does not establish full caller/lifecycle/platform coverage.
- Latest broader runtime evidence is100 staged/cached/maintenance/Clear tests on efc61d18, including actual last-state private cleanup on normal return and unwind; earlier4a3a3bc2 also has18 WAL/lifecycle/restart tests and54 compiler fixtures. These broader results predate this final API migration and must not be presented as current-head full validation.
- Latest production Dylint is74 unresolved diagnostics on efc61d18; no final migration-head production Dylint/full gate/review or full OS/feature validation has passed. Pending:remaining exclusion/cancellation/async-lane findings, short commit watchdogs, legacy unlink detach phase, private resident-owner/shutdown/checkpoint root lifetimes, opaque/generic and mapped/owned guard coverage, full local gate/platform checks and review. Child #1964 and parent #1960 remain open; #1965–1971 are unimplemented. No merge or post-merge rebase is being performed for this draft.

### Resume — finish PR #1978 (2026-10-24)

- Production Dylint 74 → 0 with no suppressions. Root causes: private plan `Drop`/`cleanup` ran `remove_dir_all` (now a rename into `<staging>/.trash` or a send to the staging sweeper thread); a failed directory unpack swept under its payload lease (temp now returned and removed after release); shutdown swept depfiles under the drain (guard dropped first). The other findings were lint imprecision: every `dyn` drop was unknown, so `Arc<SharedState>` (tokio hooks) looked like a sweep. `dyn_sources.rs` now resolves trait objects to this crate's unsizing sources, with a new `dyn_drop` fixture. Lint UI: 45/45.
- Fixed CI regressions: stale source-contract test paths; `put_racing_clear_namespace_never_errors` (clear now succeeds after detach when the retired-tree sweep is incomplete; deterministic RED→GREEN test added).
- Review (1 HIGH, 1 MEDIUM): HIGH fixed (dsymutil bundle roots beside the output now go to the sweeper via `PrivateRootRetirer`). MEDIUM accepted: if a rename fails (e.g. an open Windows handle), the root stays until its staging root is removed.
- Full daemon-core suite 1187 passed; focused cleanup/link suites pass after the follow-up.
- CI follow-up: Windows junctions in staged trash were removed with `remove_file` (no-follow `is_dir()` is false for a junction); now `remove_dir` first. Two failures pre-existing on `main` fixed: macOS stall monitor treated unavailable session CPU as perpetual progress (observable tree ticks now decide); Windows ARM `probe_spawn_guard` tests self-exec the test binary instead of PowerShell. `put_racing_clear_namespace_never_errors` hung Linux x86 twice (not reproducible locally, 1–N cores); cause: clears now sweep outside the gate, so four unthrottled writers outpaced each sweep until every clear spent its full 30 s `PROBE` budget (100 × 30 s). The writers are now capped at 2,000 puts each, and a clearer panic still raises their stop flag.
- Still open from the handoff list (not done here): short commit-holder watchdogs, legacy unlink classification, async blocking-lane enforcement, resident-owner/shutdown root-lifetime audit.
