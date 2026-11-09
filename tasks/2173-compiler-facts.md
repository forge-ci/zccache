# #2173 compiler-fact investigation (incomplete)

Branch: `fix/2173-verbatim-native-archives`, base `438838d0`.
No production code changed. The regressions in `library_search.rs` remain RED;
do not ship them as a completed fix.

## Reproduction

The new `verbatim_parent_archive` cases compile a real rlib with
`#[link(name="../outside/libchoice.a", kind="static", modifiers="+verbatim")]`
and an empty `native/` directory supplied through `-Lnative=native`.
The macro case produces the same attribute through `macro_rules!`.
Archive fixtures use old mtimes. A direct compiler control links the changed
archive and returns 91; the original archive returns 17. The removal case
renames the archive and checks the direct compiler fails before checking the
cached response. Run:

```sh
ZCCACHE_TEST_ISOLATED=1 soldr cargo test -p zccache --test cache_rust verbatim_parent_archive -- --nocapture --test-threads=1
```

Full diagnostic build/test output: `target/2173-red-final.log` (untracked).

## Compiler-fact candidates checked

- Existing `library_metadata.rs` uses `-Zls=root`; its parsed model contains
  crate identity, target triple, and Rust dependencies, not native libraries.
- Pinned rustc `-Zls=all` on the emitted rlib lists crate info, external
  dependencies, language items, features, and items. It lists the foreign
  function but does not print its native link attribute or selected archive.
  Evidence: `target/2173-metadata.txt`.
- `--emit=dep-info` omits the selected native archive.
- `-Zbinary-dep-depinfo` also omits it, both with dep-info-only emission and
  actual binary linking. It adds Rust/sysroot dependencies instead.
  Evidence: `target/2173-{binary,linked}-probe.d`.
- `--print=native-static-libs` produces no list for an rlib. For a staticlib,
  it lists libraries required by downstream links and excludes bundled
  static archives. A successful staticlib probe with `-Zno-codegen` still
  does not name this bundled outside archive.
- The emitted rlib's archive member list has `lib.rmeta`, the crate object,
  and `choice.o`; it does not retain the selected outside archive path.

## Unresolved boundary

No compiler-fact candidate checked provides the selected bundled source
archive path before admitting a hit. Source text/expanded-attribute guesses
and parent-directory inventories are prohibited by the handoff. Refusing all
native-search compilations would violate the required unchanged Cargo/cc-rs
cache-hit behavior. No such workaround has been implemented.

Resume by establishing a compiler-owned native-library fact interface and
its freshness/producer contract, then connect that to `native_inputs.rs` and
the existing content-verification/refusal machinery. The native library
records exist inside rustc metadata, but the existing rustc inspection CLI
does not expose them; decoding private metadata was not implemented.

## Final observed state

Corrected run: all three cases reach the intended assertion with
`exit=0 cached=true stale_bytes=true`; direct controls passed first.
Result: 0 passed, 3 failed, 78 filtered, test runtime 5.56 seconds.
The initial run used an invalid control rlib basename in the replacement
cases; the final run fixes that fixture error (`libcontrol.rlib`).

Formatting and `git diff --check` passed before the basename-only correction.
`./lint` completed its workspace clippy phase, then was stopped during Dylint
preparation when this investigation ended without a production implementation.
It did not pass the complete lint command. No local gate, commit, push, PR,
or GREEN validation was completed. Changes are deliberately left uncommitted
in the prepared worktree for resumption.

## Output-side follow-up (coordinator guidance)

RED regression tests committed separately: `d0cc7f0e`.

Empirical rlib member listings in `target/2173-output-facts/`:

```text
libordinary.rlib:
  lib.rmeta
  libordinary.provenance.9973d4a6b46e4333-cgu.0.rcgu.o
  choice.o
libescaping.rlib:
  lib.rmeta
  libescaping.provenance.9973d4a6b46e4333-cgu.0.rcgu.o
  choice.o
```

Both `choice.o` members and `choice.o` inside `native/libchoice.a` have
SHA256 `2aa77c3b71b43b7a294053a3b6b324412e14b8afd97742de10e0c95d6a1d2f7d`.
One source declares `name="choice", kind="static"`; the other declares
`name="../outside/libchoice.a", kind="static", modifiers="+verbatim"`.
The inventory contains an exact copy of the outside archive. Therefore an
output-member match proves equality of the bytes at store time, but cannot
prove that rustc selected the inventoried path. Changing only the outside
archive afterwards does not invalidate the matching inventoried file.
A store guard that accepts matching members would still be unsound.

A fourth real-compiler regression,
`matching_inventoried_members_do_not_certify_outside_archive`, captures this
case by copying the original outside archive into `native/` before priming
and mutating only the outside copy afterwards. Diagnostic output is retained
in `target/2173-matching-inventory-red.log`.

The daemon-core crate has no existing ar member parser dependency; existing
`require_regular_archive` checks only the global archive magic. A new parser
would not solve the provenance ambiguity above.

Stable `--print=link-args` works on pinned rustc. At the same `-o` path,
adding the print produces byte-identical executable output (`cmp` passes),
without another compilation. Evidence: `target/2173-linkargs.txt`, executable
`target/2173-linkargs`, and no-print copy `target/2173-linkargs-control`.
Its native operand is `-l:../outside/libchoice.a`, not the resolved archive
path. It can support refusing a binary store on path-bearing native operands,
but does not supply bundled rlib provenance and cannot complete this fix alone.

The coarse proposed rule shares the member-match ambiguity. Refusing every
output with bundled native members would avoid that ambiguity, but would also
refuse ordinary Cargo/cc-rs archives, violating the required unchanged HIT.
No production store guard was added that would pretend content matches
certify which external path rustc read.
