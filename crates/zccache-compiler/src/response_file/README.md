# response_file

Compiler-specific formatting for response files — the `@file` argument
indirection used when a command line would otherwise exceed the OS limit.

Each compiler family quotes and escapes differently, so a single formatter
would silently corrupt arguments for one of them.

## Layout

- `limits.rs` — when to spill: Windows above ~30,000 command-line bytes;
  Unix when one argument reaches Linux `MAX_ARG_STRLEN` (128 KiB) or the argv
  passes a quarter of `ARG_MAX`. Unix needs this because a cargo
  `rustc @argfile` is expanded before rustc runs (#2210).
- `temporary.rs` — shared temporary-file creation and ownership. Literal
  Rust `@` arguments use one-argument files so the compiler cannot expand
  them again; other argument values retain their bytes and ordering.
- `family.rs` — reading: which grammar each compiler family splits an `@file`
  with. rustc takes one argument per line, verbatim, and does not expand
  nested `@file`s (cargo's argfiles depend on this); every other family uses
  the GCC/Clang parser in `response_file.rs`. The Rust reader rejects
  non-UTF-8 input rather than accepting MSVC's UTF-16 encoding.
- `format.rs` — `gnu` plus the `*_if_safe` variants (`gnu_if_safe`,
  `msvc_if_safe`, `rustc_if_safe`). The `_if_safe` forms return `None` rather
  than emitting a response file when an argument cannot be represented
  losslessly in that family's grammar, so the caller falls back to a direct
  command line instead of shipping a mangled one.
