"""zccache's local gate (zackees/ci.yml#166, GATE-001..010).

One command that is a superset of the remote Linux quick gate, so a PR passes
CI on its first push and an attested head can skip the remote jobs it already
ran (local-gate.toml [gate.trust], ci-attestations.yml):

    uv run --no-project --python 3.13 python ci/local_gate.py             # every lane
    uv run --no-project --python 3.13 python ci/local_gate.py --lane lint # = the remote Formatting job
    uv run --no-project --python 3.13 python ci/local_gate.py --list

Do not call it directly before pushing; call it through the attesting
wrapper, which runs it on a clean tree and stamps HEAD with a tree-bound
`Local-Gate:` trailer plus one `Ci-Attestation:` trailer per proven gate:

    uvx --from git+https://github.com/zackees/ci.yml@<CI_LINT_REF> ci-lint local-gate run

Lanes (GATE-007: each is cached on its own input set):

- `lint`: every check the remote `Formatting` job (ci.yml `fmt`) runs. That
  job runs exactly `ci/local_gate.py --lane lint` and nothing else
  (GATE-001), so this list is the single source of truth for it.
- `py-tests`: the `ci/tests` pytest suite (python-tests.yml `ci/tests`).
- `check`: the MSRV job through Bosn Actions, including workspace check
  and its nested Dylint cache contract.
- `docs`: rustdoc with warnings denied (ci.yml `docs`).
- `dylint`: both default and product-feature jobs from ci.yml, through Bosn.
- `tests`: the Integration (Linux) workflow replayed through `bosn ci run`,
  never on the host (GATE-005). Every successful receipt must
  prove this clean workspace/HEAD and completed required job steps (GATE-009).

Remote-only (not attested): native macOS and Windows runs, the btrfs reflink
e2e, the miss-overhead timing budget, and every other workflow.

Checks in a lane run in parallel; each one's output is shown only when it
fails, then a timing table. Exit 1 when any check fails.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import TypeAlias

ROOT = Path(__file__).resolve().parent.parent
# The zackees/ci.yml commit whose ci_lint this repository uses. The `ci-mode`
# jobs in ci.yml and integration.yml check out the same SHA;
# ci/tests/test_local_gate.py keeps them in step.
CI_LINT_REF = "41fcbca60a7901c8707411c4abf4d0b41dd71426"
LANES = ("lint", "py-tests", "check", "docs", "dylint", "tests")
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
    # Oldest version of argv[0] that runs this check correctly.
    min_version: tuple[int, ...] | None = None
    # GATE-009: require a clean source-bound Bosn receipt and actual steps.
    bosn_workflow: str = ""
    bosn_job: str = ""
    selected_job: str | None = None
    required_steps: tuple[str, ...] = ()
    # The event the replayed workflow must have run under (`--trigger`).
    bosn_event: str = "pull_request"


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
            "MSRV check and nested Dylint (isolated Bosn Actions)",
            (
                "bosn",
                "ci",
                "run",
                "--workspace",
                ".",
                "--workflow",
                ".github/workflows/ci.yml",
                "--job",
                "msrv",
                "--trigger",
                "pr",
                "--wait",
                "--json",
            ),
            "check",
            exclusive=True,
            min_version=(0, 1, 12),
            bosn_workflow=".github/workflows/ci.yml",
            bosn_job="msrv",
            selected_job="msrv",
            required_steps=(
                "Install compiler ownership Clippy",
                "Check MSRV",
                "Verify nested Dylint cache contract",
                "Verify ignored Cargo cache regressions (#2002/#2117)",
                "Enforce compiler spawn ownership (clippy disallowed-methods)",
                "Rust argfile execution regressions",
                "Fetch-lock expiry regression",
            ),
        ),
        # #2202: replay both parallel feature selections in the same lane.
        # A partial proof must never skip either remote Dylint job.
        Check(
            "Dylint (isolated Bosn Actions)",
            (
                "bosn",
                "ci",
                "run",
                "--workspace",
                ".",
                "--workflow",
                ".github/workflows/ci.yml",
                "--job",
                "dylint",
                "--trigger",
                "pr",
                "--wait",
                "--json",
            ),
            "dylint",
            exclusive=True,
            min_version=(0, 1, 12),
            bosn_workflow=".github/workflows/ci.yml",
            bosn_job="dylint",
            selected_job="dylint",
            required_steps=(
                "Run Dylint",
                "Prove custom-lint selection for every OS",
            ),
        ),
        Check(
            "Dylint product features (isolated Bosn Actions)",
            (
                "bosn",
                "ci",
                "run",
                "--workspace",
                ".",
                "--workflow",
                ".github/workflows/ci.yml",
                "--job",
                "dylint-product",
                "--trigger",
                "pr",
                "--wait",
                "--json",
            ),
            "dylint",
            exclusive=True,
            min_version=(0, 1, 12),
            bosn_workflow=".github/workflows/ci.yml",
            bosn_job="dylint-product",
            selected_job="dylint-product",
            required_steps=(
                "Run Dylint product features",
            ),
        ),
        Check(
            "rustdoc",
            ("soldr", "cargo", "doc", "--workspace", "--no-deps"),
            "docs",
            (*RUST_ENV, ("RUSTDOCFLAGS", "-D warnings")),
            exclusive=True,
        ),
    ]
    # GATE-005/009: act sets CI=true in its job containers; the nextest
    # guard still refuses developer-host execution. Inspect the immutable
    # run receipt before allowing either real workflow to prove this lane.
    tests = [
        Check(
            "Integration (Linux, isolated Bosn Actions)",
            (
                "bosn",
                "ci",
                "run",
                "--workspace",
                ".",
                "--workflow",
                ".github/workflows/integration.yml",
                # #2039 removed `pull_request` from integration.yml, so a `pr`
                # replay skips every job and act exits 1. `push` is the event
                # the workflow still runs on; off `main` it writes no cache.
                "--trigger",
                "push",
                "--wait",
                "--json",
            ),
            "tests",
            exclusive=True,
            min_version=(0, 1, 12),
            bosn_workflow=".github/workflows/integration.yml",
            bosn_job="integration",
            bosn_event="push",
            required_steps=(
                "Build integration test binaries",
                "Install PCH regression toolchain",
                "Strict PCH regressions (Clang 21)",
                "Test (full workspace)",
                "Nextest per-test termination proof (#1971)",
                "Wrapper daemon-unavailable contract (exit 125)",
                "Strict artifact-layout validation",
                "Audit isolated integration cache",
            ),
        ),
    ]
    return lint + py_tests + rust + tests


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


def tool_version(tool: str) -> tuple[int, ...] | None:
    proc = run_captured([tool, "--version"])
    match = re.search(r"(\d+)\.(\d+)\.(\d+)", proc.output)
    return tuple(int(part) for part in match.groups()) if match else None


JsonValue: TypeAlias = (
    str | int | float | bool | list["JsonValue"] | dict[str, "JsonValue"] | None
)


@dataclass(frozen=True)
class BosnSection:
    name: str
    stage: str
    status: str
    conclusion: str | None


@dataclass(frozen=True)
class BosnJob:
    job_id: str
    status: str
    conclusion: str | None
    sections: tuple[BosnSection, ...]


@dataclass(frozen=True)
class BosnProof:
    workspace: str
    sha: str
    state: str
    conclusion: str
    engine: str
    event: str
    workflow: str
    selected_job: str | None
    exit_code: int
    jobs: tuple[BosnJob, ...]
    # The run-level failure reason, when the receipt carries one.
    reason: str | None = None


def _document(value: JsonValue) -> dict[str, JsonValue]:
    """Validate an object at the external JSON boundary."""
    if not isinstance(value, dict):
        raise TypeError("expected a JSON object")
    return value


def _values(value: JsonValue) -> list[JsonValue]:
    if not isinstance(value, list):
        raise TypeError("expected a JSON array")
    return value


def _text(value: JsonValue) -> str:
    if not isinstance(value, str):
        raise TypeError("expected a JSON string")
    return value


def _optional_text(value: JsonValue) -> str | None:
    return None if value is None else _text(value)


def _proof_job(value: JsonValue) -> BosnJob:
    job = _document(value)
    sections: list[BosnSection] = []
    for section_value in _values(job["sections"]):
        section = _document(section_value)
        sections.append(
            BosnSection(
                _text(section["name"]),
                _text(section["stage"]),
                _text(section["status"]),
                _optional_text(section["conclusion"]),
            )
        )
    return BosnJob(
        _text(job["job_id"]),
        _text(job["status"]),
        _optional_text(job["conclusion"]),
        tuple(sections),
    )


def _parse_bosn_proof(output: str) -> BosnProof:
    documents: list[dict[str, JsonValue]] = []
    for line in output.splitlines():
        if line.startswith("{"):
            documents.append(_document(json.loads(line)))
    if len(documents) != 1:
        raise ValueError("expected exactly one Bosn run receipt")
    document = documents[0]
    if document["dirty"] is not None:
        raise ValueError("Bosn executed a dirty snapshot")
    exit_code = document["exit_code"]
    if type(exit_code) is not int:
        raise ValueError("missing terminal exit code")
    tree = _document(document["tree"])
    jobs: list[BosnJob] = []
    for value in _values(tree["groups"]):
        group = _document(value)
        jobs.extend(_proof_job(job) for job in _values(group["jobs"]))
    return BosnProof(
        _text(document["workspace"]),
        _text(document["sha"]),
        _text(document["state"]),
        _text(document["conclusion"]),
        _text(document["engine"]),
        _text(document["event"]),
        _text(document["workflow"]),
        _optional_text(document["job"]),
        exit_code,
        tuple(jobs),
        _optional_text(document.get("reason")),
    )


def _job_proves_steps(job: BosnJob, required_steps: tuple[str, ...]) -> bool:
    if job.status != "completed" or job.conclusion != "success":
        return False
    completed = {
        section.name
        for section in job.sections
        if section.stage == "Main"
        and section.status == "completed"
        and section.conclusion == "success"
    }
    return set(required_steps).issubset(completed)


# act2 cannot give a called reusable workflow a qualified execution identity,
# so a run whose workflow calls one (`uses:` at job level) ends with
# conclusion="incomplete" and a nonzero exit code even though act itself
# exited 0 and every job completed successfully. Treating that as a gate
# failure makes the gate permanently un-green on any repo with a reusable
# workflow, which blocks the attestation and therefore every remote skip.
#
# The narrow exemption below is safe because the per-job evidence is still
# required to stand on its own: every job this check selected must have
# completed successfully AND run every required step (checked below). A
# receipt is only exempted when it names exactly this reason -- any other
# "incomplete", or a failed job, still fails closed.
REUSABLE_WORKFLOW_LIMITATION = "reusable workflows require qualified execution identity"


def _run_finished(proof: BosnProof) -> bool:
    """Whether the run receipt may be trusted to carry job evidence.

    A normal run must have finished successfully. A run that ended
    "incomplete" solely because of act2's reusable-workflow limitation is
    accepted only if the receipt says so and act itself exited 0.
    """

    if proof.state != "done":
        return False
    if proof.conclusion == "success" and proof.exit_code == 0:
        return True
    return proof.conclusion == "incomplete" and proof.reason == REUSABLE_WORKFLOW_LIMITATION and proof.exit_code == 3


def bosn_proof_error(
    output: str,
    *,
    workspace: Path,
    head_sha: str,
    workflow: str,
    expected_job: str,
    selected_job: str | None,
    required_steps: tuple[str, ...] = (),
    expected_event: str = "pull_request",
) -> str | None:
    """Fail closed on wrong source, unknown evidence or unexecuted checks."""
    if not required_steps:
        return "no required Bosn job steps were declared"
    try:
        proof = _parse_bosn_proof(output)
    except (KeyError, ValueError, TypeError) as error:
        return f"invalid Bosn source proof: {error}"
    if (
        not Path(proof.workspace).is_absolute()
        or Path(proof.workspace).resolve() != workspace.resolve()
    ):
        return "Bosn executed another workspace"
    if proof.sha != head_sha:
        return "Bosn executed another commit"
    if not _run_finished(proof):
        return "Bosn run did not finish successfully"
    if proof.engine != "act" or proof.event != expected_event:
        return "Bosn used another engine or event"
    if proof.workflow != workflow or proof.selected_job != selected_job:
        return "Bosn executed another workflow selection"
    jobs = [job for job in proof.jobs if job.job_id == expected_job]
    if not jobs or not all(_job_proves_steps(job, required_steps) for job in jobs):
        return "Bosn did not execute every required job step successfully"
    return None


def bosn_fidelity_error(host_arch: str, daemon: Captured) -> str | None:
    """Only native x64 Linux containers can prove these Linux test lanes."""
    if host_arch.lower() not in {"x86_64", "amd64"}:
        return "Bosn Linux tests require a native x64 host CPU"
    if daemon.returncode != 0:
        return "cannot determine Docker daemon architecture"
    if daemon.output.strip().lower() not in {"linux x86_64", "linux amd64"}:
        return "Bosn Linux tests require a Linux x64 Docker daemon"
    return None


def _epoch(value: JsonValue) -> float | None:
    if not isinstance(value, str):
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


def bosn_timing_summary(output: str, *, top: int = 8) -> str | None:
    """Where a Bosn replay spent its wall clock: queue wait, run time and the
    slowest job steps, read from the receipt. A replay can take 6x its usual
    time on a contended host or a cold cache; this makes a slow lane show
    which step grew without opening `bosn ci show`. Best effort: a receipt it
    cannot read yields None, never a gate failure."""
    try:
        document = _document(json.loads(next(line for line in output.splitlines() if line.startswith("{"))))
        created, started, finished = (document.get(k) for k in ("created_at", "started_at", "finished_at"))
        steps: list[tuple[float, str]] = []
        for group in _values(_document(document["tree"])["groups"]):
            for job in _values(_document(group)["jobs"]):
                job_doc = _document(job)
                for section in _values(job_doc["sections"]):
                    sec = _document(section)
                    begin, end = _epoch(sec.get("started_at")), _epoch(sec.get("completed_at"))
                    if begin is not None and end is not None:
                        steps.append((end - begin, f"{_text(job_doc['job_id'])}: {_text(sec['name'])}"))
    except (StopIteration, KeyError, ValueError, TypeError):
        return None
    if not all(isinstance(v, (int, float)) for v in (created, started, finished)):
        return None
    assert isinstance(created, (int, float)) and isinstance(started, (int, float)) and isinstance(finished, (int, float))
    lines = [
        f"  bosn run {document.get('id', '?')}: queued {started - created:.0f}s, "
        f"ran {finished - started:.0f}s (act {document.get('act_version', '?')})"
    ]
    for seconds, name in sorted(steps, reverse=True)[:top]:
        lines.append(f"  {seconds:7.0f}s  {name}")
    return "\n".join(lines)


def bosn_failure_summary(output: str, code: int | None) -> str:
    """Parse colored nextest output without mistaking a missing list for success."""
    clean = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", output)
    failures = list(dict.fromkeys(
        line.strip() for line in clean.splitlines()
        if re.search(r"\b(?:FAIL|SIGSEGV|SIGABRT|TIMEOUT|ABORT|LEAK FAIL)\s+\[", line)
    ))
    lines = ["  parsed failures:", *(f"    {line}" for line in failures)]
    if not failures:
        lines.append(f"    unknown failure (exit {code if code is not None else 'unknown'})")
    if re.search(r"\bact exit 137\b", clean) or code == 137:
        lines.append("  killed (SIGKILL/OOM?) under load; exit 137 does not identify the killer")
    return "\n".join(lines)


def _bosn_failure_details(output: str) -> str:
    """Best effort diagnostics; never changes the proof or retries a failed lane."""
    try:
        document = _document(json.loads(next(line for line in output.splitlines() if line.startswith("{"))))
        run_id = _text(document["id"])
    except (StopIteration, KeyError, ValueError, TypeError):
        return bosn_failure_summary(output, 1)
    report = _diagnostic_command(["bosn", "ci", "report", run_id, "--tail", "40", "--json"])
    try:
        failure = _document(_document(json.loads(report.output))["first_failure"])
        job, step = _text(failure["job"]), _text(failure["section"])
        value = failure.get("exit_code")
        code = value if type(value) is int else None
    except (KeyError, ValueError, TypeError):
        value = document.get("act_exit_code")
        code = value if type(value) is int else None
        return (bosn_failure_summary(output, code)
                + f"\n  failure report unavailable (exit {report.returncode}):\n{report.output[-12000:]}"
                + "\n  receipt/output tail:\n" + "\n".join(output.splitlines()[-40:])[-12000:])
    logs = _diagnostic_command(["bosn", "ci", "logs", run_id, "--job", job, "--step", step])
    tail = "\n".join(logs.output.splitlines()[-40:])[-12000:]
    return (
        f"  failing step: {job}: {failure.get('step', step)} (exit {code})\n"
        + bosn_failure_summary(output + "\n" + logs.output, code)
        + f"\n  failing step log tail (last 40 lines):\n{tail}"
    )


def _diagnostic_command(argv: list[str]) -> Captured:
    with tempfile.TemporaryFile() as out:
        try:
            proc = subprocess.run(argv, cwd=ROOT, stdin=subprocess.DEVNULL,
                                  stdout=out, stderr=subprocess.STDOUT, check=False, timeout=60)
            code = proc.returncode
        except (OSError, subprocess.TimeoutExpired) as error:
            return Captured(1, f"diagnostics unavailable: {error}")
        out.seek(0)
        return Captured(code, out.read().decode("utf-8", errors="replace"))


def _host_context(label: str) -> str:
    sampled_at = time.time()
    try:
        load = "/".join(f"{value:.2f}" for value in os.getloadavg())
    except (AttributeError, OSError):
        load = "unavailable"
    runners = _diagnostic_command(["bosn", "ci", "runners", "list", "--json"])
    try:
        state = _document(_document(json.loads(runners.output))["runners"])
        concurrency = f"{state.get('running', '?')}/{state.get('limit', '?')} running/limit; queued={state.get('queued', '?')}"
    except (KeyError, TypeError, ValueError):
        concurrency = "unavailable"
    return f"  host sample ({label}, epoch {sampled_at:.0f}): load1/5/15={load}; runners={concurrency}\n"


def _run(check: Check) -> Result:
    head = None
    if check.bosn_workflow:
        daemon = run_captured(
            ["docker", "info", "--format", "{{.OSType}} {{.Architecture}}"]
        )
        error = bosn_fidelity_error(platform.machine(), daemon)
        if error:
            return Result(check, 1, 0.0, f"local gate: {error}\n")
        head = run_captured(["git", "rev-parse", "HEAD"])
        if head.returncode != 0:
            return Result(check, 1, 0.0, "cannot determine this checkout's HEAD")
    context = _host_context("before replay") if check.bosn_workflow else ""
    result = _run_plain(check)
    if not check.bosn_workflow:
        return result
    if head is None or head.returncode != 0:
        error = "cannot determine this checkout's HEAD"
    else:
        error = bosn_proof_error(
            result.output,
            workspace=ROOT,
            head_sha=head.output.strip(),
            workflow=check.bosn_workflow,
            expected_job=check.bosn_job,
            selected_job=check.selected_job,
            required_steps=check.required_steps,
            expected_event=check.bosn_event,
        )
    return Result(
        check,
        1 if error else 0,
        result.seconds,
        result.output + (f"\nlocal gate: {error}\n" + context
                         + _host_context("after failed replay")
                         + _bosn_failure_details(result.output) + "\n" if error else ""),
    )


def _run_plain(check: Check) -> Result:
    start = time.monotonic()
    tool = shutil.which(check.argv[0])
    if tool is None:
        return Result(check, 127, 0.0, f"{check.argv[0]}: not found on PATH")
    if check.min_version is not None:
        found = tool_version(tool)
        if found is None or found < check.min_version:
            want = ".".join(map(str, check.min_version))
            return Result(
                check,
                1,
                0.0,
                f"{check.argv[0]} {found or 'unknown'} is older than {want}; "
                f"upgrade it: uv tool upgrade {check.argv[0]}",
            )
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
    summary = bosn_timing_summary(result.output) if result.check.bosn_workflow else None
    if summary:
        print(summary, flush=True)


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
    sys.exit(main())
