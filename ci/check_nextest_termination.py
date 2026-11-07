#!/usr/bin/env python3
"""Prove nextest's per-test termination, and that its budget filters are live (#1971).

The hang check runs the planted tests in crates/zccache-test-support/tests/test_child.rs
under the short `hang-proof` profile. It passes only when the hanging test times
out by name with its diagnostic kept, the unrelated test still passes, and every
descendant is gone. The filter check fails on any override in the default
profile that selects no test, since a dead filter silently removes its budget.
Run after the workspace is built, under CI=true or ZCCACHE_TEST_ISOLATED=1.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HANG_TIMEOUT_S = 300
LIST_TIMEOUT_S = 300
DESCENDANT_GRACE_S = 30
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def nextest(subcommand: str, features: str, *args: str) -> list[str]:
    return [
        "soldr", "cargo", "nextest", subcommand,
        "--workspace", "--features", features, *args,
    ]


def pid_alive(pid: int) -> bool:
    """A zombie is dead for this purpose: it holds no pipe and runs nothing."""
    try:
        stat = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8")
    except OSError:
        return False
    return stat.rsplit(")", 1)[1].split()[0] != "Z"


def hang_failures(output: str, returncode: int, pids: dict[str, int]) -> list[str]:
    """Return every way a hang-proof run failed the termination contract."""
    failures = []
    if returncode == 0:
        failures.append("the run passed, so the planted hang was never terminated")
    if not re.search(r"TIMEOUT \[.*planted_hang_is_terminated_by_name", output):
        failures.append("no TIMEOUT line names planted_hang_is_terminated_by_name")
    if "PLANTED-DIAGNOSTIC" not in output:
        failures.append("the hanging test's diagnostic output was not retained")
    if not re.search(r"PASS \[.*planted_unrelated_test_still_passes", output):
        failures.append("the unrelated planted test did not pass")
    for name in ("raw", "owned"):
        if name not in pids:
            failures.append(f"the planted test recorded no {name} descendant pid")
    deadline = time.monotonic() + DESCENDANT_GRACE_S
    while any(pid_alive(pid) for pid in pids.values()) and time.monotonic() < deadline:
        time.sleep(0.1)
    failures += [
        f"{name} descendant {pid} survived termination"
        for name, pid in pids.items()
        if pid_alive(pid)
    ]
    return failures


def check_hang(features: str) -> list[str]:
    with tempfile.TemporaryDirectory(prefix="zccache-plant-") as plant_dir:
        result = subprocess.run(
            nextest(
                "run", features,
                "--profile", "hang-proof",
                "--run-ignored", "only",
                "--no-fail-fast",
                "-E", "test(planted_)",
            ),
            cwd=ROOT,
            env={**os.environ, "ZCCACHE_TEST_PLANT_DIR": plant_dir},
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=HANG_TIMEOUT_S,
            check=False,
        )
        print(result.stdout)
        pids = {
            path.stem: int(path.read_text(encoding="utf-8"))
            for path in Path(plant_dir).glob("*.pid")
        }
    return hang_failures(ANSI.sub("", result.stdout), result.returncode, pids)


def override_filters(config: dict) -> list[str]:
    overrides = config["profile"]["default"].get("overrides", [])
    return [entry["filter"] for entry in overrides if "filter" in entry]


def check_filters(features: str) -> list[str]:
    config = tomllib.loads((ROOT / ".config/nextest.toml").read_text(encoding="utf-8"))
    failures = []
    for expression in override_filters(config):
        result = subprocess.run(
            nextest("list", features, "--message-format", "oneline", "-E", expression),
            cwd=ROOT,
            stdout=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=LIST_TIMEOUT_S,
            check=False,
        )
        if result.returncode != 0:
            failures.append(f"could not list filter {expression!r}")
        elif not result.stdout.strip():
            failures.append(f"filter {expression!r} matches no test")
        else:
            count = len(result.stdout.splitlines())
            print(f"filter {expression!r} matches {count} tests")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--features", required=True)
    args = parser.parse_args()
    failures = check_filters(args.features) + check_hang(args.features)
    for failure in failures:
        print(f"nextest termination check: {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
