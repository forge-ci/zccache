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

Timeout-only enforcement (#1967) also checks resolved optional-duration budget
signatures, optional millisecond waits, semaphore admission, kernal-api runtime
bridges, child capture and optional-timeout command runners. Aliases, UFCS and
function-item references resolve to the same denied definitions. `ci/lint.py`
runs this library uncapped for every supported OS with all inventoried product
features, independently of the shrinking baseline for unrelated feature lints.
Workspace clippy selects the same product features. Runtime regressions remain
necessary: the lint does not prove that a callback or kernel syscall terminates.

For the product-feature negative proof, create an empty snapshot directory with
`safe-mktemp /tmp/zccache-timeout-proof`, then run
`uv run --no-project python -m ci.prove_timeout_features /tmp/zccache-timeout-proof`.
It plants waits in the real product sources and calls the production lint
entrypoint for each OS. Review the retained `proof-logs/` before removing the
snapshot with `safe-rm -r /tmp/zccache-timeout-proof`.
