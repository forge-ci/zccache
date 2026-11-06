# ban_raw_network_access

Dylint library (#1968): HTTP transports may only be used inside
`crates/zccache-download/src/http/`, whose API makes connect, header and stall
budgets and cancellation mandatory. It rejects any path or method that resolves
into `reqwest`, `hyper`, `hyper_util` or `ureq`, by definition rather than by
spelling, so aliases, UFCS and function items cannot bypass it. A trait method
instantiated on one of their types (`Default::default` for `reqwest::Client`) is
rejected too.

Types that those crates only re-export, such as the `http` crate's header names
and `StatusCode` or `url::Url`, are not transports and stay allowed. A lint cannot
prove that a request terminates; `zccache-download`'s stalled-socket tests cover
that.

- `src/` holds the lint.
- `ui/` holds the UI fixtures run by `cargo test`.
