"""Keep local gate proofs in step with the hosted jobs they cover."""

from __future__ import annotations

import re
import tomllib
from pathlib import Path

import yaml

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
    config = tomllib.loads((ROOT / "ci.toml").read_text(encoding="utf-8"))
    assert config["linter"] == f"zackees/ci.yml@{ref}"
    trust = tomllib.loads((ROOT / "local-gate.toml").read_text())["gate"]["trust"]
    assert {"ci.toml", "push"} <= set(trust["surfaces"])
    for workflow, job in (
        ("ci.yml", "ci-mode"),
        ("ci.yml", "cache-janitor"),
        ("ci-linux.yml", "reuse-shadow"),
    ):
        steps = yaml.safe_load(_job_block(workflow, job))[job]["steps"]
        assert [
            step["with"]["ref"]
            for step in steps
            if step.get("with", {}).get("repository") == "zackees/ci.yml"
        ] == [ref], (workflow, job)


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


def test_product_clippy_1967_is_in_the_real_lint_lane() -> None:
    argvs = [c.argv for c in local_gate.checks() if c.lane == "lint"]
    assert ("uv", "run", "--no-project", "python", "-m", "ci.lint", "--clippy-only") in argvs


def test_skippable_jobs_consume_their_ci_mode_decision() -> None:
    for workflow, job in (
        ("ci.yml", "fmt"),
        ("ci.yml", "docs"),
        ("ci.yml", "dylint"),
        ("ci.yml", "dylint-product"),
        ("ci.yml", "clippy"),
    ):
        block = _job_block(workflow, job)
        output = f"skip_{job}"
        assert f"needs.ci-mode.outputs.{output} != 'true'" in block, (workflow, job)
        assert "ci-mode" in block.split("if:", 1)[0], (workflow, job)


def test_both_dylint_jobs_require_the_complete_attested_lane() -> None:
    attestations = yaml.safe_load((ROOT / "ci-attestations.yml").read_text())
    trust = tomllib.loads((ROOT / "local-gate.toml").read_text())["gate"]["trust"]
    for name in ("dylint", "dylint-product"):
        key = f"ci.yml:{name}"
        assert key in trust["skip"]
        assert trust["covered-by"][key] == ["rust-lint"]
        gates = attestations["jobs"][key]
        assert gates and all(attestations["gates"][g]["lane"] == "rust-lint" for g in gates)
        output = f"skip_{name}"
        mode = _job_block("ci.yml", "ci-mode")
        assert output in mode
        # Pinned ci-lint output_name preserves the job id, including hyphens.
        if "-" in name:
            assert f"{output}: ${{{{ steps.gate.outputs['skip_{name}'] }}}}" in mode


def test_clippy_skip_requires_the_complete_attested_lane() -> None:
    policy = yaml.safe_load((ROOT / "ci-attestations.yml").read_text())
    trust = tomllib.loads((ROOT / "local-gate.toml").read_text())["gate"]["trust"]
    mode = yaml.safe_load(_job_block("ci.yml", "ci-mode"))["ci-mode"]
    key = "ci.yml:clippy"
    assert key in trust["skip"]
    assert trust["covered-by"][key] == ["rust-lint"]
    assert [policy["gates"][gate]["lane"] for gate in policy["jobs"][key]] == ["rust-lint"]
    assert mode["outputs"]["skip_clippy"] == "${{ steps.gate.outputs.skip_clippy }}"
    assert mode["permissions"]["pull-requests"] == "read"
    verify = next(step for step in mode["steps"] if step.get("id") == "gate")
    assert verify["env"]["GITHUB_TOKEN"] == "${{ github.token }}"


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


def test_termination_proof_reuses_the_integration_build_environment() -> None:
    workflow = yaml.safe_load(
        (WORKFLOWS / "integration.yml").read_text(encoding="utf-8")
    )
    steps = {
        step.get("name"): step for step in workflow["jobs"]["integration"]["steps"]
    }
    built = steps["Test (full workspace)"]["env"]
    proof = steps["Nextest per-test termination proof (#1971)"]
    for key in (
        "SOLDR_CACHE_DIR",
        "SOLDR_CACHE_LIFECYCLE",
        "SOLDR_CACHE_SHUTDOWN_TIMEOUT_SECS",
        "SOLDR_TARGET_BLOCK_FREE_GB",
        "CARGO_BUILD_JOBS",
        "SOLDR_JOBS",
    ):
        assert proof.get("env", {}).get(key) == built[key], key
    assert "unset ZCCACHE_CACHE_DIR" in proof["run"]


def test_rustc_argfile_regressions_run_in_hosted_ci() -> None:
    block = _job_block("ci.yml", "msrv")
    assert "name: Rust argfile execution regressions" in block
    assert "soldr rustup which rustc" in block
    assert (
        "soldr cargo test -p zccache --features test-support --test cache_rust "
        "daemon_rustc_response_file_test:: -- --ignored --test-threads=1"
    ) in block


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


def test_soldr_lint_covers_the_skipped_lint_jobs() -> None:
    """ci.yml dylint/dylint-product and clippy.yml clippy skip on the
    `rust-lint` proof, so `soldr lint` must run what they run."""
    metadata = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["metadata"]
    assert metadata["dylint"]["libraries"] == [{"path": "dylints/*"}]
    config = metadata["soldr"]["lint"]
    assert config["steps"] == ["fmt", "clippy", "dylint"]
    extras = [" ".join(extra) for extra in config["extra"]]
    for job, workflow, command in (
        ("dylint", "ci.yml", "ci/check_dylint_wiring.py"),
        ("dylint", "ci.yml", "ci.lint --dylint-libraries"),
        ("dylint", "ci.yml", "ci.lint --dylint-only --pass cross"),
        ("dylint", "ci.yml", "ci.lint --dylint-fixture"),
        ("dylint-product", "ci.yml", "ci.lint --dylint-only --pass product"),
        ("clippy", "clippy.yml", "ci.lint --clippy-hosted"),
    ):
        assert any(command in extra for extra in extras), command
        assert _job_block(workflow, job)
    clippy = _job_block("clippy.yml", "clippy")
    assert "name: Enforce compiler spawn ownership (clippy disallowed-methods)" in clippy
    assert "clippy" not in _job_block("ci.yml", "msrv").lower().replace(
        "clippy.toml", ""
    )


def test_hosted_test_jobs_do_not_depend_on_local_attestations() -> None:
    gate = tomllib.loads((ROOT / "local-gate.toml").read_text())["gate"]
    attestations = yaml.safe_load((ROOT / "ci-attestations.yml").read_text())
    assert set(gate["lanes"]) == {check.lane for check in local_gate.checks()}
    assert all(check.argv[0] != "bosn" for check in local_gate.checks())
    assert all("bosn" not in lane["tools"] for lane in gate["lanes"].values())
    for workflow, job in (("ci.yml", "msrv"), ("ci.yml", "linux"),
                          ("integration.yml", "integration")):
        key = f"{workflow}:{job}"
        assert key not in gate["trust"]["skip"]
        assert key not in attestations["jobs"]
        assert f"skip_{job}" not in _job_block(workflow, job)
