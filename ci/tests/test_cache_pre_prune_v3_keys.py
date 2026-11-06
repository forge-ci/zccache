"""setup-soldr#627 key formats in the cache transition forecast (CACHE-026).

The floating ``setup-soldr@v0`` moved cook bases and build caches to v3. Run
42171585061 failed closed on the first v3 key, disabling main cache writes.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

from ci import check_cache_budget as budget

ROOT = Path(__file__).resolve().parents[2]
LOCK = "6dddc4f9c75e8293"
LOCK_CRLF = "6b16881a68a68514"
OLD_LOCK = "c215d7208a6cddb6"
SOLDR = "se71dd067ebacc28b"

# Main-ref v3 entries of the live inventory at run 42171585061.
LIVE_V3: list[tuple[str, int]] = [
    (
        f"cook-base-v3-linux-x64-glibc-rustc1.95.0-f9e7e4902-l{LOCK}-soldrv0.9.51",
        749_286_657,
    ),
    (
        f"cook-base-v3-linux-x64-glibc-rustc1.95.0-f6cafa616-l{LOCK}-soldr0.9.51",
        278_758_719,
    ),
    (
        f"cook-base-v3-linux-x64-glibc-rustc1.95.0-fnone-l{LOCK}"
        "-xcheck-linux-x86-musl-soldrv0.9.51",
        573_045_421,
    ),
    (
        f"setup-soldr-buildcache-v3-windows-x64-94f0dac836d70212-{LOCK_CRLF}-{SOLDR}",
        374_467_472,
    ),
    (
        f"setup-soldr-buildcache-v3-linux-x64-8ae7e3e10fed3308-check-linux-arm-musl-{LOCK}-{SOLDR}",
        476_101_327,
    ),
]
OLD_V2_BUILD = f"setup-soldr-buildcache-v2-linux-x64-c97cbff348d78f86-check-linux-x86-musl-{OLD_LOCK}"


def _node(expr: str, payload: object) -> dict:
    script = (
        "const p=require(process.argv[1]);"
        "const i=JSON.parse(require('node:fs').readFileSync(0,'utf8'));"
        f"process.stdout.write(JSON.stringify(({expr})(p,i)));"
    )
    result = subprocess.run(
        ["node", "-e", script, str(ROOT / "ci/cache_cleanup_plan.js")],
        input=json.dumps(payload),
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def _rows(entries: list[tuple[str, int]]) -> list[dict]:
    return [
        {
            "id": index + 1,
            "key": key,
            "ref": "refs/heads/main",
            "size_in_bytes": size,
            "created_at": "2026-11-06T05:50:00Z",
        }
        for index, (key, size) in enumerate(entries)
    ]


def _plan(entries: list[tuple[str, int]]) -> dict:
    caches = _rows(entries)
    total = sum(size for _, size in entries)
    return _node(
        "(p,i)=>p.planLockTransitionPrePrune(i.caches,"
        f"{{linux:'{LOCK}',macos:'{LOCK}',windows:['{LOCK_CRLF}']}},i.total,i.total)",
        {"caches": caches, "total": total},
    )


def _cook(key: str) -> dict | None:
    return _node("(p,i)=>({parts:p.parseCookBaseKey(i)})", key)["parts"]


def _build(key: str) -> dict | None:
    return _node("(p,i)=>({parts:p.setupSoldrBuildCacheKeyParts(i)})", key)["parts"]


def test_live_v3_inventory_forecasts_instead_of_failing_closed() -> None:
    """RED before the fix: `unknown cache key format: cook-base-v3-...`."""
    x86_musl = f"setup-soldr-buildcache-v3-linux-x64-b6333e686e3919f1-check-linux-x86-musl-{LOCK}-{SOLDR}"
    plan = _plan(LIVE_V3 + [(x86_musl, 476_074_843), (OLD_V2_BUILD, 476_784_918)])
    assert plan["ok"], plan.get("reason")
    # Every current-lock v3 entry stays; only the old-lock v2 build its v3
    # sibling replaced is deleted.
    assert plan["deleteIds"] == [7]


def test_v3_cook_key_parses_soldr_last_with_optional_suffix() -> None:
    plain = _cook(LIVE_V3[1][0])
    assert plain is not None
    assert (plain["flags"], plain["lockHash"], plain["soldr"], plain["suffix"]) == (
        "6cafa616",
        LOCK,
        "0.9.51",
        "",
    )
    suffixed = _cook(LIVE_V3[2][0])
    assert suffixed is not None
    assert (suffixed["flags"], suffixed["soldr"], suffixed["suffix"]) == (
        "none",
        "0.9.51",
        "xcheck-linux-x86-musl",
    )


def test_v3_build_key_separates_suffix_lock_and_soldr_digest() -> None:
    parts = _build(LIVE_V3[4][0])
    assert parts is not None
    assert (parts["digest"], parts["suffix"], parts["lockHash"]) == (
        "8ae7e3e10fed3308",
        "check-linux-arm-musl",
        LOCK,
    )
    bare = _build(LIVE_V3[3][0])
    assert bare is not None
    assert (bare["suffix"], bare["lockHash"]) == ("", LOCK_CRLF)


def test_truly_unknown_keys_still_fail_closed() -> None:
    bad = [
        f"cook-base-v4-linux-x64-glibc-rustc1.95.0-fnone-l{LOCK}-soldrv0.9.51",
        # v3 layout must not accept the v2 segment order, or vice versa.
        f"cook-base-v3-linux-x64-glibc-rustc1.95.0-fnone-l{LOCK}-soldrv0.9.51-xdylint",
        f"cook-base-v3-linux-x64-glibc-rustc1.95.0-fnone-l{LOCK}",
        f"setup-soldr-buildcache-v4-linux-x64-8ae7e3e10fed3308-{LOCK}-{SOLDR}",
        # v3 requires the trailing soldr digest.
        f"setup-soldr-buildcache-v3-linux-x64-8ae7e3e10fed3308-{LOCK}",
    ]
    for key in bad:
        plan = _plan([(key, 1_000)])
        assert not plan["ok"], key
        assert "unknown" in plan["reason"], plan["reason"]
        assert plan["retireFirstIds"] == [], key


def test_v3_cook_supersedes_same_family_v2_even_at_older_soldr() -> None:
    """A v3 consumer's restore prefix can never match a v2 entry."""
    v2 = f"cook-base-v2-linux-x64-glibc-rustc1.95.0-f9e7e4902-l{LOCK}-soldrv0.9.60"
    plan = _plan([(LIVE_V3[0][0], 749_286_657), (v2, 700_000_000)])
    assert plan["ok"], plan.get("reason")
    assert plan["retireFirstIds"] == [2]


def test_old_lock_v3_generations_are_retired_like_v2() -> None:
    old_cook = (
        f"cook-base-v3-linux-x64-glibc-rustc1.95.0-f6cafa616-l{OLD_LOCK}-soldr0.9.51"
    )
    old_build = (
        f"setup-soldr-buildcache-v3-linux-x64-8ae7e3e10fed3308-{OLD_LOCK}-{SOLDR}"
    )
    plan = _plan([(old_cook, 1_000), (old_build, 1_000)])
    assert plan["ok"], plan.get("reason")
    assert set(plan["staleCookIds"]) == {1}


def test_off_linux_v3_cook_bases_stay_retired() -> None:
    key = f"cook-base-v3-windows-x64-msvc-rustc1.95.0-fnone-l{LOCK}-soldrv0.9.51"
    assert _node("(p,i)=>({r:p.isRetiredMainKey(i)})", key)["r"] is True


def test_family_budgets_count_v3_entries() -> None:
    rows = _rows([(LIVE_V3[0][0], budget.FAMILY_BUDGETS["cook-base"] + 1)])
    errors = budget.evaluate_families(rows)
    assert errors and "family cook-base" in errors[0]
    rows = _rows([(LIVE_V3[4][0], budget.FAMILY_BUDGETS["build-cache"] + 1)])
    errors = budget.evaluate_families(rows)
    assert errors and "family build-cache" in errors[0]
