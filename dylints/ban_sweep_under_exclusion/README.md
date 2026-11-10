# Sweep ownership and exclusion

Introduced by #1964.

The lint resolves raw tree deletion DefIds and confines them to explicit sweep,
link-removal and private staging/lifecycle owners. Function-item capture is
rejected even in approved owners to prevent exporting a raw capability.
Raw recursive deletion has sweep effects even inside approved private cleanup
owners. Publication readers and materialization exclusion permit bulk copying;
recursive deletion is forbidden while either remains held.

The separate ownership role includes `CacheRootWriterLease`. Resolved leaks,
generic ownership erasure and raw overwrites of that capability are rejected.
It can span an async admission wait and owned blocking cleanup. Its role adds
no publication/store critical section, and the existing opaque staging-root
boundary is preserved for ownership analysis.

Guard analysis uses compiler move initialization, guard field paths and actual
control-flow edges, with enum variant refinement. The compiler fixtures cover
aliases, partial moves, released wrappers and real core publication roles.
Drop effects also follow uniquely initialized enum values through aggregate
fields and whole-value moves. Arguments, ambiguous writes, pointer/reference
aliases and unsupported projections do not establish a known-variant proof.
On its private MIR clone, guard analysis removes drop edges only when compiler
initialization proves that no initialized field requires destruction. This
prevents fully moved temporaries from creating impossible unwind paths. Live
custom destructors and partially moved values with remaining drop fields keep
their unwind edges; the compiler's original MIR is never changed.

Instantiated helper summaries propagate ownership leaks and ownership-erasing
casts, including generic `forget` and `Box<dyn Any>` helpers. Non-owning markers
do not acquire ownership merely by naming a permit type. Coroutine cancellation
includes captured and saved-local destructors; a direct coroutine drop following
a proven `Poll::Ready` uses a separate completion proof. Custom futures retain
their destructor effects after `Ready`.

Owned transfers into checked synchronous local helpers use the callee's explicit
guard parameter types and move paths. Other caller-held guards remain live.
Generic parameters, borrowed guards, foreign callees, async callees and opaque
or guard-owning return values do not establish this transfer contract. The
callee remains subject to guard checks, including destructor and unwind paths.
The canonical `kernal_api::async_engine::launch_blocking` handoff additionally
accepts checked local closures when capture destruction releases exclusion
before forbidden cleanup on cancellation. Direct guards and structural
Option/Result/tuple wrappers have release proofs; shared or manually suppressed
owners do not. The invoked closure body is still checked independently, and
other caller-held guards remain live. Async task launches have no such contract.

Automatic struct/tuple/enum destruction uses declared field order when every
live owned guard belongs to the dropped value. Each possible variant must
release its guards before cleanup; application custom destructors and borrowed
or separately held guards retain normal checks. The exact private staged
materialization lease facade has a reader role at its boundary; each lease
holder is tracked without claiming that no other reader exists. Generic shared
wrappers still do not establish an automatic release proof.
The same proof applies to a moved value passed to resolved `core::mem::drop`,
including aliases and function items. Borrowed values and unknown function
pointers do not establish ownership transfer.
Resolved `ptr::drop_in_place`, `ManuallyDrop::drop`, and
`MaybeUninit::assume_init_drop` propagate the destroyed type's cleanup effects.
They do not establish a moved-owner release proof for the caller's guards.

Dropping a trait object runs the destructor of a value unsized into it. Its
effects are the drop effects of this crate's unsizing coercions and casts to
the same principal trait and generic arguments, so `dyn Fn(&TaskMeta)` and
`dyn Fn()` resolve separately. A local closure or function item passed to a
foreign generic function also counts for a matching `Fn`-family object, since
foreign code (tokio runtime hooks, for example) may box it internally. A
generic source is instantiated through the local uses of its function, up to
eight generic callers; an exported generic function stays unknown. Other local
types unsized only inside foreign generic code are not tracked.

`ZCCACHE_DYLINT_EXPLAIN=1` adds notes naming each finding's effects, callee or
dropped value, and held guards. `ZCCACHE_DYLINT_EXPLAIN_TRACE=<file>` appends
the destructors, opaque drops and depth limits that produced unknown effects;
cargo does not forward raw driver output. Both are diagnostics only.

The core publication facade distinguishes blocking commit permits from complete
admission operations and async checkpoint/shutdown drains. The production
workspace gate passes. Cancellation glue with retained guards, blocking-lane
enforcement, the remaining ownership audit, and final OS/feature checks remain
unfinished.
