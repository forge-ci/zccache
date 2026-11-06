# http

The only module allowed to touch `reqwest` (enforced by the
`ban_raw_network_access` Dylint, #1968).

- `mod.rs`: `HttpClient`, `HttpRequest`, `HttpResponse`, the mandatory
  `HttpPolicy` budgets and URL redaction.
- `tests.rs`: behaviour tests against local sockets that stall on purpose.
