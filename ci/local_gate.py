"""zccache's local gate (zackees/ci.yml#166, GATE-001..010).

One command that is a superset of the remote Linux quick gate, so a PR passes
CI on its first push and an attested head can skip the remote jobs it already
ran (local-gate.toml [gate.trust], ci-attestations.yml):

    uv run --no-project --python 3.13 python ci/local_gate.py             # every lane
    uv run --no-project --python 3.13 python ci/local_gate.py --lane lint # = the remote Formatting job
    uv run --no-project --python 3.13 python ci/local_gate.py --list

Use `./push` to run the lanes on a clean tree and stamp HEAD with a
tree-bound `Local-Gate:` trailer and one `Ci-Attestation:` per proven gate.

Lanes (GATE-007: each is cached on its own input set):

- `lint`: every check the remote `Formatting` job (ci.yml `fmt`) runs. That
  job runs exactly `ci/local_gate.py --lane lint` and nothing else
  (GATE-001), so this list is the single source of truth for it.
- `py-tests`: the `ci/tests` pytest suite (python-tests.yml `ci/tests`).
- `docs`: rustdoc with warnings denied (ci.yml `docs`).
- `rust-lint`: `soldr lint rust --check` on the host -- fmt, Clippy, Dylint
  and the Cargo.toml `[workspace.metadata.soldr.lint]` extras. It covers
  ci.yml's `dylint` and `dylint-product` jobs and clippy.yml's `clippy` job.
  Lint never runs under bosn.
GitHub Actions runs MSRV, Linux unit tests, and Integration (Linux).

Checks in a lane run in parallel; each one's output is shown only when it
fails, then a timing table. Exit 1 when any check fails.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import tempfile
import time
import tomllib
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# Workflow checkouts use the same pin (guarded by ci/tests/test_local_gate.py).
CI_LINT_REF = tomllib.loads((ROOT / "ci.toml").read_text(encoding="utf-8"))[
    "linter"
].split("@", 1)[1]
LANES = ("lint", "py-tests", "docs", "rust-lint")
PY = ("uv", "run", "--no-project", "--python", "3.13")
# ci.yml's and integration.yml's workflow-level env: warnings are errors. It
# also replaces .cargo/config.toml's build.rustflags, exactly as on CI.
RUST_ENV = (("RUSTFLAGS", "-D warnings"),)


@dataclass(frozen=True)
class Check:
    name: str
    argv: tuple[str, ...]
    lane: str
    env: tuple[tuple[str, str], ...] = ()
    # Run after every parallel check, alone: it saturates every core.
    exclusive: bool = False


def checks() -> list[Check]:
    lint = [
        Check(
            "rustfmt",
            ("soldr", "cargo", "fmt", "--all", "--", "--check"),
            "lint",
            RUST_ENV,
        ),
        Check(
            "Clippy (default and shipped product features)",
            ("uv", "run", "--no-project", "python", "-m", "ci.lint", "--clippy-only"),
            "lint",
            RUST_ENV,
        ),
        # A workspace build unifies features, which hides a module gated on the
        # wrong feature; only a build of this feature alone can catch it (#2107).
        Check(
            "zccache-cli-core builds with only download-client",
            (
                "soldr",
                "cargo",
                "check",
                "-p",
                "zccache-cli-core",
                "--no-default-features",
                "--features",
                "download-client",
            ),
            "lint",
            RUST_ENV,
        ),
        Check(
            "Strict benchmark metric type check (pyright)",
            ("uvx", "--from", "pyright==1.1.411", "pyright"),
            "lint",
        ),
        Check(
            "Benchmark report contract tests",
            (
                *PY,
                "--with",
                "pytest",
                "--with",
                "pillow",
                "pytest",
                "-q",
                "ci/tests/test_benchmark_metrics.py",
                "ci/tests/test_benchmark_stats.py",
            ),
            "lint",
            (("PYTHONPATH", "."),),
        ),
        Check(
            "Log-audit registry snapshot",
            ("uv", "run", "python", "ci/check_log_audit_registry.py"),
            "lint",
        ),
        Check(
            "setup-soldr cache footprint",
            (
                "uv",
                "run",
                "--no-project",
                "--with",
                "pyyaml",
                "python",
                "ci/check_cache_footprint.py",
            ),
            "lint",
        ),
        Check(
            "kernal-api migration inventory",
            ("uv", "run", "--no-project", "python", "ci/check_kernal_api_baseline.py"),
            "lint",
        ),
        Check(
            "Local gate wiring (GATE-001..010)",
            (
                "uvx",
                "--from",
                f"git+https://github.com/zackees/ci.yml@{CI_LINT_REF}",
                "--with",
                "pyyaml",
                "ci-lint",
                "local-gate",
                "lint",
                "--repo",
                ".",
            ),
            "lint",
        ),
    ]
    py_tests = [
        Check(
            "ci/tests",
            (
                *PY,
                "--with",
                "pytest",
                "--with",
                "pillow",
                "--with",
                "pyyaml",
                "python",
                "-m",
                "pytest",
                "ci/tests",
                "-q",
                "-rs",
            ),
            "py-tests",
            (("PYTHONPATH", "."),),
        )
    ]
    rust = [
        Check(
            "rustdoc",
            ("soldr", "cargo", "doc", "--workspace", "--no-deps"),
            "docs",
            (*RUST_ENV, ("RUSTDOCFLAGS", "-D warnings")),
            exclusive=True,
        ),
    ]
    # Lint never runs under bosn. `soldr lint` runs fmt, Clippy and the host
    # Dylint pass, then the Cargo.toml extras: clippy.yml's hosted features
    # and spawn ownership, the dylints/* library tests, the cross-OS and
    # product-feature Dylint passes and the per-OS selection proof. `--lib
    # --bins` is the Dylint scope ci.lint has always used (#1740); Clippy
    # keeps `--all-targets`.
    rust_lint = [
        Check(
            "soldr lint (fmt, Clippy, Dylint and repo extras)",
            ("soldr", "lint", "rust", "--check", "--lib", "--bins"),
            "rust-lint",
            exclusive=True,
        ),
    ]
    return lint + py_tests + rust + rust_lint


@dataclass(frozen=True)
class Result:
    check: Check
    code: int
    seconds: float
    output: str


@dataclass(frozen=True)
class Captured:
    returncode: int
    output: str


def run_captured(argv: list[str], env: dict[str, str] | None = None) -> Captured:
    """Run `argv` from the repository root with stdout and stderr captured
    through one temporary file, never a pipe (zackees/ci.yml PY-003): a full
    pipe blocks the child, and a daemon that inherits a pipe keeps the caller
    waiting for an EOF that never comes."""
    with tempfile.TemporaryFile() as out:
        proc = subprocess.run(
            argv,
            cwd=ROOT,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=out,
            stderr=subprocess.STDOUT,
            check=False,
        )
        out.seek(0)
        return Captured(proc.returncode, out.read().decode("utf-8", errors="replace"))


def _run(check: Check) -> Result:
    start = time.monotonic()
    tool = shutil.which(check.argv[0])
    if tool is None:
        return Result(check, 127, 0.0, f"{check.argv[0]}: not found on PATH")
    env = {**os.environ, **dict(check.env)} if check.env else None
    proc = run_captured(list(check.argv), env)
    return Result(check, proc.returncode, time.monotonic() - start, proc.output)


def _save_log(result: Result) -> Path:
    logs = ROOT / "target" / "local-gate-logs"
    logs.mkdir(parents=True, exist_ok=True)
    path = logs / (
        re.sub(r"[^A-Za-z0-9_.-]+", "-", result.check.name).strip("-") + ".log"
    )
    path.write_text(result.output, encoding="utf-8")
    return path


def _report(result: Result) -> None:
    status = "ok  " if result.code == 0 else "FAIL"
    print(f"{status} {result.seconds:6.1f}s  {result.check.name}", flush=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--lane", choices=("all", *LANES), default="all", help="one GATE-007 lane"
    )
    parser.add_argument("--list", action="store_true", help="print the checks and exit")
    parser.add_argument("--jobs", type=int, default=min(8, os.cpu_count() or 2))
    args = parser.parse_args(argv)

    selected = [c for c in checks() if args.lane in ("all", c.lane)]
    if args.list:
        for check in selected:
            env = " ".join(f"{k}={v!r}" for k, v in check.env)
            print(
                f"[{check.lane}] {check.name}: {env + ' ' if env else ''}{' '.join(check.argv)}"
            )
        return 0

    start = time.monotonic()
    results: list[Result] = []
    with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as pool:
        futures = [pool.submit(_run, c) for c in selected if not c.exclusive]
        for future in as_completed(futures):
            result = future.result()
            results.append(result)
            _report(result)
    for check in (c for c in selected if c.exclusive):
        result = _run(check)
        results.append(result)
        _report(result)

    failed = [r for r in results if r.code != 0]
    for result in failed:
        print(f"\n===== FAIL: {result.check.name} (exit {result.code}) =====")
        print(f"$ {' '.join(result.check.argv)}")
        print(result.output.rstrip()[-20000:])
        # The tail can miss the cause (nextest prints a failing test's output
        # where it fails, not at the end), so keep the whole log.
        print(f"full output: {_save_log(result)}")
    total = time.monotonic() - start
    print(
        f"\nlocal gate ({args.lane}): {len(results) - len(failed)}/{len(results)} passed in {total:.0f}s"
    )
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
