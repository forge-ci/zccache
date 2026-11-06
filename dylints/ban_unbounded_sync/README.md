# Bounded synchronization

`ban_unbounded_sync` rejects raw blocking waits and lock acquisition outside the product boundaries. `ban_guard_across_await` checks the compiler coroutine layout for retained data guards, including aliases guards nested in generic containers, and concrete local wrappers. Async state uses `zccache_core::sync::State`; unit-only coordination uses `sync::Gate` and `sync::Barrier`, whose raw primitives stay private.

Native `blocking_sync` guards retain holder-watchdog tickets and remain data
guards, including unit-valued native locks. Their resolved types are rejected
across suspension even when renamed or nested in a concrete caller wrapper.
Only the private core watchdog may use its raw bookkeeping mutex; operation
permits retain their distinct data-free contract.

The `examples/` UI fixtures compile against the real core dependency and cover
native guard aliases, wrapper fields, UFCS/function-item acquisition bypasses,
and allowed callback state, synchronous guards and async operation permits.
The UI harness gets actual dependency paths from Cargo's JSON artifacts, keeping
soldr's wrapper enabled rather than relying on bare compiler command lines.
Compiler UI mode includes these example sources; normal product scope remains
restricted to production crate sources.
