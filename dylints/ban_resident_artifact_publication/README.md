# Resident artifact publication boundary

Issue #1965: resident constructors belong to the provisional publication owner.
The owner exposes separate provisional and durable publication operations and
checks identity before replacing a live entry. This deny-level lint resolves
constructor DefIds, including aliases, UFCS, and function-item references.
Runtime tests cover release, failure, and stale persistence completion; the lint
does not prove those runtime properties. Test crates may construct fixture entries.
