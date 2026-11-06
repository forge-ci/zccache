# zccache-download

Internal download engine and shared download types for zccache.

## HTTP boundary

`zccache_download::http` is the only product code allowed to use `reqwest`,
enforced by the `ban_raw_network_access` Dylint (#1968). Every request takes a
mandatory `HttpPolicy` and a cancellation token. `CONTROL` caps finite exchanges;
`TRANSFER` has no elapsed-time limit, so a slow download survives as long as bytes
keep arriving. The module docs justify each value. Errors omit URL userinfo and
query strings.
