# ban_raw_subprocess_in_daemon

Deny raw subprocess creation and session access in daemon production code.
The matcher resolves DefIds, including aliases, UFCS, and stored function items.

- std/Tokio Command spawn, output, status and Child waits/termination are banned.
- Native platform creation APIs and creation_flags are banned.
- kernal-api SpawnSpec spawn/spawn_session, native foreground/sync spawn APIs,
  ProcessSession/PlatformChild/SpawnedChild/DaemonChild waits,
  pipe operations, sampling and termination, and bounded-command capture must
  remain inside the canonical process_session.rs owner.
- A function using those semantic capabilities inside the owner must require
  ProcessLiveness or Duration directly. Optional budgets do not qualify.
- Callers use the daemon process facade with mandatory ProcessLiveness. Short
  synchronous probes may use zccache_core::wait::output_for with a Duration.

The production scope is crates/zccache-daemon-core/src/daemon/. Test builds and
fixture files are exempt; production targets are linted separately. UI mode
checks all fixture sources. There is no std/Tokio or platform-spawn allowlist.
The exact canonical session owner is the sole semantic capability exception.

The owner applies priority, admission, native containment, concurrent drains,
progress monitoring, finite total/post-exit/cleanup deadlines and cancellation
cleanup. The lint enforces capability placement and explicit budget signatures;
runtime regression tests establish liveness behavior.

The examples fixtures link the actual kernal-api and bounded core dependency.
Their artifact flags come from Cargo JSON, preserving the soldr compiler wrapper.
