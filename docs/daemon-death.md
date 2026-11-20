# Investigating daemon death during native builds (#2064)

**Verdict: not proven.** Neither the retained OBS logs nor these local probes
identify why the reported connections closed. In particular, they do not prove
that the daemon process died. This diagnostic change does not resolve #2064 or
establish that its cause is outside zccache's control.

Read the existing investigation first:

- [zccache #2064](https://github.com/forge-ci/zccache/issues/2064), including
  [the retained-evidence analysis](https://github.com/forge-ci/zccache/issues/2064#issuecomment-6624808092).
- [OBS draft #3613](https://github.com/obs-rust/obs-studio/pull/3613) preserves
  completed compiler RSS/termination records and sanitized lifecycle events.
  This observer complements it: interrupted requests never produce completed
  journal records, and SIGKILL cannot run an in-process crash handler.

## Connection errors are observations, not causes

These locations match both the reported **1.15.5** tag (`61b4233b`) and the
investigated main base (`bb9a62ce`), relative to `crates/`:

| Location | Meaning |
| --- | --- |
| `zccache-cli-core/src/cli/commands/wrap/ipc.rs:887` | A compile response is `None`: constructs `ClosedConnection` with `lost connection to daemon (no response)`. |
| Same file, `:249` | Formats `zccache[err][R]: daemon response failure (closed-connection): ...`. |
| Same file, `:335` | Receive returns an IPC error: constructs `broken connection to daemon: {e}`; a peer reset reaches this path. |
| Same file, `:942` | Equivalent no-response case for link requests. |

An unwinding request-task panic, a framing/I/O error, or connection cancellation
can drop a connection while the daemon remains alive. On current main,
`zccache-daemon-core/src/daemon/server/run.rs:224` launches detached connection
tasks; ordinary handler errors are logged at `:226`.
`server/connection/mod.rs:317` receives a request and can close/error before
dispatch; `:127` cancels dispatch when the client disconnects. Preserve daemon
stderr and panic reports even when no process exit is observed.

## Process termination inventory

Locations below are current-main paths under `crates/`; the entrypoint locations
also match 1.15.5. Reachability is a possibility, not attribution to OBS.

| Termination class | Implementation / evidence | Reachable during concurrent PCH work? |
| --- | --- | --- |
| Kernel OOM / external SIGKILL / runner or container teardown | Outside the process; no Rust hook can report SIGKILL. Need parent wait status **and** kernel victim PID/start-time evidence. | Yes. OOM remains an admission hypothesis, not an explanation established by peer resets. |
| Allocation failure, allocator abort, native fault, stack overflow, double panic | Multi-call binary allocator at `zccache/src/bin/zccache.rs:19`; daemon installs crash capture at `zccache-daemon-core/src/daemon/entry.rs:258`; panic hook at `zccache-core/src/crash.rs:334`, native capture at `:380`. | Yes in principle. Need raw crash spool/dump, stderr, signal and stack. No allocation-failure evidence retained in OBS. A panic report alone does not prove process exit. |
| Uncaught top-level panic | Entrypoint `entry.rs:247` owns server startup/runtime. Cargo profiles do not set `panic = "abort"`; a request-task unwind need not kill the daemon. | Possible, but needs a stack. Background supervision in `server/supervise.rs:116` logs/restarts selected tasks; it does not imply every panic kills the process. |
| Fatal synchronization watchdog | `zccache-core/src/sync_watchdog.rs:45` aborts after failed acquisition; `:191` detects overdue short holders, `:98` calls `process::abort`. Reporter/thread startup failure also aborts. | Reachable on current main under contention or stuck holders. **Absent from 1.15.5**, so not a cause of the reported version's failure. |
| Explicit startup exit | `entry.rs:306,405` defer to an existing daemon (exit 0); `:413,419` reject bind/join failure; `:483` rejects inability to record ownership. | These are startup exits of a contender, not a mechanism for killing an already-serving process mid-compile. |
| Server returns error | `entry.rs:641–647` logs server error then exits 1. | Possible; requires the server-error log. Normal accept errors at `server/run.rs:218` continue accepting. |
| Requested graceful shutdown | `server/connection/dispatch.rs:69` handles shutdown; `entry.rs:603,613` install interrupt/TERM shutdown; successful `run` returns at `:650`. | Yes if an external actor requests it. OBS evidence places setup cleanup **after** the disconnect, so that cleanup does not explain the earlier failure. |
| Idle / private-owner lifecycle | `server/maintenance_schedule.rs:506` checks idle timeout; `:538` checks private owners and requests shutdown when none remain. | Possible if configured/ownership lost, with `died-idle` / `died-private-owner-exit` events. The ordinary idle default is 3600 s; active requests refresh activity, but idle is not an in-flight memory admission test. |
| Compiler liveness watchdog / accept watchdog / memory eviction | Compiler ownership is separate from daemon lifetime; `server/run.rs:208` observes accept stalls; `run_memory_eviction_pass` at `:780` evicts state. | These do not themselves call daemon `process::exit`/`abort`. Child termination normally produces a compiler verdict; it is not daemon death. |

An exhaustive signal-number list would not distinguish senders: default-action
signals and native faults terminate through OS/runtime mechanisms above. Preserve
the actual signal and kernel/supervisor attribution instead of guessing a sender.

## Admission exposure, not a demonstrated root cause

In 1.15.5, `server/compile_resource_gate.rs:200` classifies native units by known
amalgamation filename or source size >= 1,000,000 bytes (`:213`). The equivalent
current classifier is at `:197–215`. A tiny CMake PCH stub including a huge header
tree is ordinary shared work. Neither classifier measures its include closure or
compiler-tree memory. Several such units may compile concurrently.

Existing PCH detection lives in `server/handle_compile/pch_gate.rs` and
`handle_compile_multi_preflight.rs`; reuse that boundary for a future admission
fix if measurements establish the need. This PR changes no admission policy.
An observed OOM would be a scheduling/admission defect to fix with oversized-unit
or measured-memory admission, not a justification for a global job cap or retry.

## Local probes (not an OBS reproduction)

On 2026-11-04 UTC, used existing repo harness image
`zccache-standalone-perf:1`, image ID
`sha256:1adc164a9b5ab590a8acec4ef0255200d81b48034bf8bead57be7fd2b0d39c4a`.
Mounted this checkout read-only plus an isolated evidence directory, disabled
networking, set four CPUs, and used equal memory/memory-swap limits (zero swap).
The published Linux x86_64 musl 1.15.5 archive was verified against release
SHA256SUMS: `39906c92b0287e2aac51ae9169b368d36eabc3af363c3c7bd29dc3783975e579`.

Each attempt used a fresh cache, endpoint, foreground daemon and extended session.
Three simultaneous cold requests each compiled a distinct header containing only
`#include <bits/stdc++.h>` using `/usr/bin/c++ -O2 -g -std=c++20 -x c++-header
headerN.hpp -o headerN.gch`. This tests concurrent native PCH work, not OBS's
CEF headers, runner contention or exact deployed soldr integration.

| Limit | Compiler statuses | Cgroup peak bytes | OOM kill delta | Daemon |
| --- | --- | --- | --- | --- |
| 2 GiB | 0, 0, 0 | 1,712,140,288 | 0 | Status succeeded; stop requested afterward; parent observed exit 0. |
| 1 GiB | 0, 1, 0 | 1,073,741,824 | 1 | Status succeeded; stop requested afterward; parent observed exit 0. |

At 2 GiB the peak sampled daemon RSS was 515,358,720 bytes; sampled daemon-tree
RSS was 1,328,865,280 bytes. Successful PCH outputs were about 153 MB each.
At 1 GiB the failed compiler reported `Killed signal terminated program cc1plus`.
This is consistent with the cgroup OOM event, but without a kernel victim report
it is not per-PID kernel attribution. It **did not** produce daemon death or
the reported no-verdict failures. Counters alone would have misdiagnosed it.

Raw local evidence is retained in `.perf-local/daemon-death/pch-{1g,2g}/`
(samples, daemon stderr, per-compiler logs, raw journal and results). The initial
invalid-session setup attempt is separate at `.perf-local/daemon-death/`; it
never reached compilation and is not included above. These are correctness /
failure probes, not performance measurements or the sanctioned timing matrix.

## Cheapest decisive next CI measurement

Use `ci/daemon_death_probe.py` as the parent of the **exact deployed daemon** in
foreground mode in an isolated job, before any build client starts it. Match
the existing job's cache root, endpoint, namespace and binary version; changing
those would observe a different daemon. Do not replace another job's daemon.
For a multi-call binary, make a symlink named `zccache-daemon` to that binary.

```bash
uv run --no-project python ci/daemon_death_probe.py \
  --output "$RUNNER_TEMP/daemon-observation" --duration-secs 14400 \
  --interval-secs 1 -- /path/to/zccache-daemon --foreground \
  --endpoint "$ZCCACHE_ENDPOINT"
```

Run that parent alongside the normal build, after ensuring no daemon already
owns the selected endpoint. Wait for the daemon's `listening for connections`
log before starting clients; the observer's `started` event only means spawn,
not readiness. Keep existing stats/journal/lifecycle collection from OBS #3613.
After collecting failure evidence, stop this job's daemon through the normal
shutdown command and wait for the observer. Its own duration expiry / interrupt
is explicitly recorded before TERM (then KILL after a 10-second cleanup bound).
SIGTERM/SIGHUP to the observer use the same bounded cleanup path. If a killed
child cannot be reaped within another ten seconds, the observer writes
`unreaped`, leaves its final status null and returns 125.
Those induced signals must not be attributed to the original build failure.

The observer fsyncs JSONL records, samples daemon/descendant RSS and identity
(PID + start ticks), host meminfo, and cgroup-v2 current/peak/limit/swap/pressure/
OOM counters at every visible ancestor. Missing measurements are null/empty,
not zero. It records the parent's actual exit/signal status and preserves raw
daemon stdout/stderr in `daemon.log`. It never retries or restarts anything.

Retain kernel OOM output for the same interval (`journalctl -k` or `dmesg`, where
permitted), crash reports **including the raw native spool**, and supervisor
termination logs. Match kernel victim PID/time to the observer's identity.
Interpretation:

- Daemon SIGKILL + kernel OOM victim matching that daemon + simultaneous memory
  peak establishes OOM attribution; use measured unit/tree memory to design admission.
- SIGABRT/native fault + stack/allocation stderr establishes a different fatal
  mechanism. An allocation failure can occur without a kernel OOM kill.
- Daemon survives the disconnect: inspect request-task panic/connection logs.
- SIGKILL alone, or a cgroup `oom_kill` increment alone, remains inconclusive.

The observer must survive to reap the child. Prefer an observer outside the
memory-constrained cgroup; a shared `memory.oom.group=1` kill may kill both, leaving
only fsynced samples. Cgroup v1, hidden ancestors, process exit between samples,
and sub-interval peaks can leave evidence unknown. RSS sums can double-count
shared pages and are not cgroup charged memory. Records cap process details at
256 entries and flag truncation. Duration is mandatory (maximum six hours).
Raw stderr can contain build paths/arguments; retain it with normal CI log access
controls and sanitize before public sharing. This PR adds no workflow or OBS
integration; the real failing job still needs to run this measurement.

## Observer validation

Real foreground processes in the same Docker image exercised ordinary exit 23,
SIGKILL (observer exit 137, recorded daemon status -9) and observer deadline
(exit 124, explicitly induced TERM). A cancellation probe against the initial
observer revision (`8546a994`) exited 143 with no final record; the corrected
observer exits 130 and records `observer-interrupt`, its own TERM and child
status -15. That RED/GREEN proves a diagnostic cleanup fix, **not** the cause
of OBS's daemon disconnects. Four Rust regressions are registered in the
existing `daemon_lifecycle` target; they require `uv` and a local Python on Linux.
