# ban_client_refusal_in_scan

Dylint library (#2027): the daemon's internal compiler scan modules
(`compiler_spawn.rs`, `pch_overlay_aliases.rs`, `pipeline/system_includes.rs`)
must not build `PchRefusal`, any `PchRefusalReason` variant, `Response::Error`
or `CompileExecResult::Error`. A scan reports a plain error, and only its
fail-open caller decides what the client sees, so a failed internal scan can
never fail a compile that would succeed (#1977).

- `src/` holds the lint.
- `ui/` holds the UI fixtures run by `cargo test`.
