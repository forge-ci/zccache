"""The local gate stays in step with the remote jobs it stands in for.

An attested PR head skips the remote Formatting, Documentation, MSRV and
Integration (Linux) jobs (local-gate.toml [gate.trust], ci-attestations.yml),
so the gate must keep running what those jobs run. These checks catch the
drift a reviewer would miss: a ci-lint pin bumped in one place only, a step
added to the mirrored job instead of the lane, or an Integration test command
the isolated lane never learned about.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

from ci import local_gate

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github" / "workflows"
GATE_TESTS = ROOT / "ci" / "docker" / "gate" / "run_gate_tests.sh"


def _job_block(workflow: str, job: str) -> str:
    text = (WORKFLOWS / workflow).read_text(encoding="utf-8")
    start = text.index(f"\n  {job}:\n")
    following = re.search(r"\n  [a-z0-9-]+:\n", text[start + 1 :])
    end = start + 1 + following.start() if following else len(text)
    return text[start:end]


def test_ci_lint_pin_is_the_same_everywhere() -> None:
    ref = local_gate.CI_LINT_REF
    assert re.fullmatch(r"[0-9a-f]{40}", ref)
    for workflow in ("ci.yml", "integration.yml"):
        block = _job_block(workflow, "ci-mode")
        assert f"ref: {ref}" in block, workflow
    assert f"ci.yml@{ref}" in (ROOT / "local-gate.toml").read_text(encoding="utf-8")


def test_formatting_job_runs_only_the_lint_lane() -> None:
    """GATE-001: every check of the mirrored job lives in the gate."""
    block = _job_block("ci.yml", "fmt")
    runs = re.findall(r"^\s+run: (.+)$", block, re.MULTILINE)
    assert runs == [
        "uv run --no-project --python 3.13 python ci/local_gate.py --lane lint"
    ]
    assert {c.lane for c in local_gate.checks()} == set(local_gate.LANES)


def test_lint_lane_builds_cli_core_with_only_download_client() -> None:
    """#2107: workspace feature unification hides this combination's breakage."""
    argvs = [c.argv for c in local_gate.checks() if c.lane == "lint"]
    assert (
        "soldr",
        "cargo",
        "check",
        "-p",
        "zccache-cli-core",
        "--no-default-features",
        "--features",
        "download-client",
    ) in argvs


def test_skippable_jobs_consume_their_ci_mode_decision() -> None:
    for workflow, job in (
        ("ci.yml", "fmt"),
        ("ci.yml", "docs"),
        ("ci.yml", "msrv"),
        ("integration.yml", "integration"),
    ):
        block = _job_block(workflow, job)
        assert f"needs.ci-mode.outputs.skip_{job} != 'true'" in block, (workflow, job)
        assert "ci-mode" in block.split("if:", 1)[0], (workflow, job)


def test_legacy_harness_runs_every_integration_test_command() -> None:
    """Each `soldr cargo test|nextest` command of the Integration (Linux) job
    on pull requests, and the MSRV job's ignored test, runs in the lane."""
    lane = GATE_TESTS.read_text(encoding="utf-8").replace('"$FEATURES"', "FEATURES")
    integration = _job_block("integration.yml", "integration")
    # The ignored/stress suite runs only on schedule and dispatch.
    integration = integration.split(
        "- name: Test ignored integration and stress suite", 1
    )[0]
    msrv = _job_block("ci.yml", "msrv")
    commands = re.findall(
        r"soldr cargo (?:test|nextest run)[^\n|]*", integration + msrv
    )
    assert len(commands) >= 5, commands
    for command in commands:
        normalized = command.replace(
            '"$ZCCACHE_INTEGRATION_FEATURES"', "FEATURES"
        ).strip()
        normalized = re.sub(r"\s*2>&1$", "", normalized).rstrip('"').strip()
        assert normalized in lane, normalized


def test_isolated_lanes_replay_the_real_workflows() -> None:
    """Every lane that attests a remote job replays that job through Bosn.

    Keyed by lane so adding a lane cannot be satisfied by reordering the ones
    already covered -- each row below is a job a PR head can now skip.
    """
    isolated = [check for check in local_gate.checks() if check.bosn_workflow]
    by_lane = {check.lane: check for check in isolated}
    assert len(isolated) == len(by_lane) == 3
    assert {
        lane: (check.bosn_workflow, check.bosn_job, check.selected_job)
        for lane, check in by_lane.items()
    } == {
        "check": (".github/workflows/ci.yml", "msrv", "msrv"),
        "dylint": (".github/workflows/ci.yml", "dylint", "dylint"),
        "tests": (".github/workflows/integration.yml", "integration", None),
    }
    assert all(check.argv[:3] == ("bosn", "ci", "run") for check in isolated)
    assert all("--wait" in check.argv and "--json" in check.argv for check in isolated)
    assert all(check.min_version == (0, 1, 12) for check in isolated)
    assert "Verify nested Dylint cache contract" in by_lane["check"].required_steps
    assert by_lane["dylint"].required_steps == (
        "Run Dylint",
        "Rust argfile execution regressions",
        "Prove custom-lint selection for every OS",
    )
    assert "Test (full workspace)" in by_lane["tests"].required_steps
    assert "Install PCH regression toolchain" in by_lane["tests"].required_steps
    assert "Strict PCH regressions (Clang 21)" in by_lane["tests"].required_steps
    for check in isolated:
        workflow = check.bosn_workflow.rsplit("/", 1)[-1]
        block = _job_block(workflow, check.bosn_job)
        for step in check.required_steps:
            assert f"name: {step}" in block


def test_rustc_argfile_regressions_run_in_attested_ci() -> None:
    block = _job_block("ci.yml", "dylint")
    assert "name: Rust argfile execution regressions" in block
    assert "soldr rustup which rustc" in block
    assert (
        "soldr cargo test -p zccache --features test-support --test cache_rust "
        "daemon_rustc_response_file_test:: -- --ignored --test-threads=1"
    ) in block
    check = next(check for check in local_gate.checks() if check.lane == "dylint")
    assert "Rust argfile execution regressions" in check.required_steps


def _receipt(sections: list[dict[str, object]]) -> str:
    return json.dumps(
        {
            "id": "run-1",
            "act_version": "0.2.89-act2.36",
            "created_at": 100.0,
            "started_at": 130.0,
            "finished_at": 730.0,
            "tree": {"groups": [{"jobs": [{"job_id": "dylint", "sections": sections}]}]},
        }
    )


def test_bosn_timing_summary_names_queue_wait_and_slowest_steps() -> None:
    summary = local_gate.bosn_timing_summary(
        _receipt(
            [
                {
                    "name": "Run Dylint",
                    "started_at": "2026-11-02T21:00:00Z",
                    "completed_at": "2026-11-02T21:03:39Z",
                },
                {
                    "name": "zackees/setup-soldr@v0",
                    "started_at": "2026-11-02T20:58:00Z",
                    "completed_at": "2026-11-02T20:59:34Z",
                },
                {"name": "never started", "started_at": None, "completed_at": None},
            ]
        )
    )
    assert summary is not None
    lines = summary.splitlines()
    assert "queued 30s, ran 600s (act 0.2.89-act2.36)" in lines[0]
    assert lines[1].split() == ["219s", "dylint:", "Run", "Dylint"]
    assert "setup-soldr" in lines[2]
    assert len(lines) == 3


def test_bosn_timing_summary_never_fails_the_gate_on_odd_output() -> None:
    assert local_gate.bosn_timing_summary("not json") is None
    assert local_gate.bosn_timing_summary('{"tree": {}}') is None


def test_failure_summary_strips_color_and_keeps_nextest_identity() -> None:
    output = '\x1b[31;1m        FAIL\x1b[0m [ 0.372s] (1282/3702) zccache-core blocking_sync::handoff_latency_tests::contended_rwlock_writer_handoff_is_sub_millisecond'
    summary = local_gate.bosn_failure_summary(output, 100)
    assert 'blocking_sync::handoff_latency_tests::contended_rwlock_writer_handoff_is_sub_millisecond' in summary
    assert '\x1b' not in summary


def test_failure_summary_never_emits_an_empty_failure_list() -> None:
    assert 'unknown failure (exit 100)' in local_gate.bosn_failure_summary('## Integration Test Failures\n```\n```', 100)


def test_failure_summary_classifies_137_without_claiming_oom() -> None:
    summary = local_gate.bosn_failure_summary('act exit 137; engine cleanup failed: act serve close failed', 1)
    assert 'killed (SIGKILL/OOM?) under load' in summary
    assert 'OOM confirmed' not in summary


def test_failed_step_diagnostics_fetches_scoped_logs(monkeypatch) -> None:
    calls = []
    def capture(argv):
        calls.append(argv)
        if 'report' in argv:
            # A valid report returns the failed run's exit status.
            return local_gate.Captured(1, json.dumps({'first_failure': {'job': 'job-key', 'section': 'Main:9', 'step': 'Test', 'exit_code': 100}}))
        return local_gate.Captured(0, 'FAIL [0.3s] crate test_name\n' + 'later doctest output\n' * 50)
    monkeypatch.setattr(local_gate, '_diagnostic_command', capture)
    summary = local_gate._bosn_failure_details(json.dumps({'id': 'run-id'}))
    assert 'FAIL [0.3s] crate test_name' in summary
    assert 'failing step: job-key: Test (exit 100)' in summary
    assert calls[1] == ['bosn', 'ci', 'logs', 'run-id', '--job', 'job-key', '--step', 'Main:9']
    assert summary.count('later doctest output') == 40
    assert 'diagnostics unavailable' not in summary


def test_integration_workflow_summary_handles_colored_nextest(tmp_path) -> None:
    import os
    import subprocess
    block = _job_block('integration.yml', 'integration')
    script = block.split('          if [ $status -ne 0 ]; then', 1)[1].split('          exit $status', 1)[0]
    (tmp_path / 'test-output.txt').write_text('\x1b[31;1m        FAIL\x1b[0m [0.3s] crate test_name\n')
    summary = tmp_path / 'summary'
    proc = subprocess.run(['bash', '-c', 'status=100\nif [ $status -ne 0 ]; then' + script], cwd=tmp_path,
                          env={**os.environ, 'GITHUB_STEP_SUMMARY': str(summary)}, timeout=10)
    assert proc.returncode == 0
    assert 'FAIL [0.3s] crate test_name' in ' '.join(summary.read_text().split())


def test_failed_report_keeps_diagnostic_error_and_unknown_status(monkeypatch) -> None:
    monkeypatch.setattr(local_gate, '_diagnostic_command', lambda argv: local_gate.Captured(1, 'daemon unavailable'))
    summary = local_gate._bosn_failure_details(json.dumps({'id': 'run-id'}))
    assert 'unknown failure (exit unknown)' in summary
    assert 'daemon unavailable' in summary
    assert 'receipt/output tail' in summary


def test_integration_workflow_unknown_failure_is_explicit(tmp_path) -> None:
    import os
    import subprocess
    block = _job_block('integration.yml', 'integration')
    script = block.split('          if [ $status -ne 0 ]; then', 1)[1].split('          exit $status', 1)[0]
    (tmp_path / 'test-output.txt').write_text('runner disappeared\n')
    summary = tmp_path / 'summary'
    subprocess.run(['bash', '-c', 'status=137\nif [ $status -ne 0 ]; then' + script], cwd=tmp_path,
                   env={**os.environ, 'GITHUB_STEP_SUMMARY': str(summary)}, timeout=10, check=True)
    assert 'unknown failure (exit 137)' in summary.read_text()
