# UI tests

`*.rs` here are minimal programs that exercise specific lint behavior. For
each, `dylint_testing::ui_test` compiles the file and asserts the resulting
diagnostics match the adjacent `*.stderr` snapshot.

- `disallowed.rs` — a `std::fs` free function call and a `std::fs::File` type
  reference. Both must trigger the lint: raw `std::fs` skips the kernal-api
  capability that owns durability, identity and permissions semantics.
