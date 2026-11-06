# ban_raw_env_flag

Denies hand-rolled environment boolean parsing outside
`crates/zccache-core/src/config/env_policy.rs` (#1969). It complements
`ban_registered_env_read`: that lint owns where a registered name is read, this
one owns how a value is interpreted.

It flags, within one function body:

- a comparison set using two or more boolean spellings (`1 0 true false yes no on off y n`), through `==`, `eq_ignore_ascii_case`, UFCS, or constants;
- a `match` or array whose string literals are all boolean spellings;
- a `"0"` or `"1"` comparison next to an environment read or an `is_empty` check.

A `match` with a non-boolean arm such as `"consistent"` is an enum mode and is
allowed. `EXEMPT_FILES` in `src/lib.rs` lists foreign vocabularies and the sites
awaiting a decision in #2119. Runtime grammar is tested in `env_policy.rs`.
