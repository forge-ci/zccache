# ban_std_fs_outside_adapters

This lint bans `std::fs` in workspace code so filesystem *mechanism* is reached
through the kernal-api capability that owns it — durable directory sync, shared
append, opaque file identity, mapped-file access, private-directory permissions
— instead of being re-derived at each call site.

The approved platform adapters
(`crates/{zccache-ipc,zccache-daemon-core,zccache-cli-core,zccache}/src/platform.rs`)
are exempt: they are where a native handle becomes a facade type. Everything
else starts denied, with the pre-existing call sites carried in
`src/allowlist.txt` and burned down as migrations land.

The capability source of truth is
`docs/architecture/kernal-api-migration.toml` (`kernel_capability` fields).
