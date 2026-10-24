# Real facade fixtures

Compiler checks use the actual core publication and artifact retirement APIs.

`raw_guard_memory` rejects unsafe pointer ownership duplication/overwrites and
vector length changes for guard-containing elements, including resolved aliases,
function items and UFCS. The same operations on bytes remain allowed.

`ordered_local_unwind` covers release after tuple destructuring, rebinding,
branching and `?`. Its negative companion retains a live destructor after a
partial move, which must keep the real unwind and cleanup violation visible.

`root_lease_ownership` rejects resolved and generic leaks, erasure, suppression
and raw overwrite of a real core root lease. `root_lease_work` permits bounded
cleanup, operation lifetimes and non-owning markers with that capability.

`dyn_drop` resolves trait-object drops to the values this crate unsized into
them: a sweeping `dyn Hook` or `dyn Fn()` is rejected, while an unrelated trait
or a different `Fn` signature stays clean. A generic boxing helper is
instantiated through its local caller.
