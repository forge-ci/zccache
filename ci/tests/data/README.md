# ci/tests/data

Recorded inputs for the cache pre-prune planner tests.

- `zccache_caches_2026-10-26.json`: the live Actions cache list (51 entries, 8.79 GB) taken while main's cache writes were frozen (2026-10-22..26). `test_cache_pre_prune_retire_first.py` replays it to prove the planner retires the superseded old-lock entries and gets back under the 9.2 GB transition target (CACHE-026).
