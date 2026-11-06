# ban_swallowed_child_stdio

Dylint library (#1970): a child's stdout and stderr must not go to a null sink,
because a helper's failure output is the evidence needed to diagnose it. It
rejects `std::process::Stdio::null` and any `StreamMode::Null` variant, resolved
semantically so aliases, UFCS and function items cannot bypass it. Closing stdin
stays valid. Capture small-tool output with `zccache_core::small_tool`.

A detached or existence-only child may opt out with a scoped
`#[allow(unknown_lints, ban_swallowed_child_stdio, reason = "...")]`. The lint
does not check the reason exists; review does. A lint also cannot see stderr that
is captured and then discarded; runtime regressions cover that, such as
`compiler_probe_diagnostics`.

- `src/` holds the lint.
- `ui/` holds the UI fixtures run by `cargo test`.
