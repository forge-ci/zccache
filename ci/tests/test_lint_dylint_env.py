import subprocess
from collections import Counter

from ci import lint


def _captured_envs(monkeypatch, configured: str | None) -> list[dict[str, str]]:
    envs: list[dict[str, str]] = []

    def fake_run(command, **kwargs):
        envs.append(kwargs["env"])
        return subprocess.CompletedProcess(command, 0)

    if configured is None:
        monkeypatch.delenv("SOLDR_DYLINT_CONFIGURED_TOOLCHAIN", raising=False)
    else:
        monkeypatch.setenv("SOLDR_DYLINT_CONFIGURED_TOOLCHAIN", configured)
    monkeypatch.setenv("SOLDR_DYLINT_TOOLCHAIN", "leaked-from-parent")
    monkeypatch.setattr(lint, "which", lambda name: f"/usr/bin/{name}")
    monkeypatch.setattr(lint, "dylint_cross_targets", lambda: [])
    monkeypatch.setattr(lint, "dylint_product_pass", lambda env, targets: 0)
    monkeypatch.setattr(lint.subprocess, "run", fake_run)
    assert lint.lint_dylint_only() == 0
    assert envs
    return envs


def test_setup_soldr_run_leaves_the_dylint_scope_to_soldr(monkeypatch) -> None:
    """SOLDR_DYLINT_TOOLCHAIN marks a *nested* Dylint scope. Under setup-soldr it
    must be absent, or soldr never writes the success marker and setup-soldr
    skips the dylint-cache and dylint-output-cache saves."""
    for env in _captured_envs(monkeypatch, "nightly-2026-08-15"):
        assert "SOLDR_DYLINT_TOOLCHAIN" not in env


def test_local_run_still_pins_the_dylint_toolchain(monkeypatch) -> None:
    for env in _captured_envs(monkeypatch, None):
        assert env["SOLDR_DYLINT_TOOLCHAIN"] == lint.DYLINT_TOOLCHAIN


def test_parse_counts_located_warnings_and_ignores_summaries() -> None:
    output = (
        "warning: raw wait\n   --> crates/a/src/x.rs:1:2\n"
        "warning: `a` (lib) generated 2 warnings\n"
        " 88.1 warning: raw wait\n  --> crates/a/src/x.rs:9:2\n"
    )
    assert lint.parse_feature_findings(output) == {("crates/a/src/x.rs", "raw wait"): 2}


def test_ratchet_rejects_new_findings_and_stale_entries() -> None:
    key = ("crates/a/src/x.rs", "raw wait")
    assert lint.compare_feature_findings(Counter({key: 1}), Counter({key: 1})) == []
    assert "new" in lint.compare_feature_findings(Counter({key: 2}), Counter({key: 1}))[0]
    assert "shrink" in lint.compare_feature_findings(Counter(), Counter({key: 1}))[0]


def test_product_pass_forces_a_real_run(monkeypatch, tmp_path) -> None:
    baseline = tmp_path / "baseline.txt"
    baseline.write_text("", encoding="utf-8")
    envs = []

    def fake_run(command, **kwargs):
        envs.append(kwargs["env"])
        return subprocess.CompletedProcess(command, 0, stdout="")

    monkeypatch.setattr(lint, "DYLINT_FEATURE_BASELINE", baseline)
    monkeypatch.setattr(lint.subprocess, "run", fake_run)
    assert lint.dylint_product_pass({}, [None]) == 0
    assert envs[0]["SOLDR_VERDICTS"] == "off"


def test_parse_handles_ci_forced_color() -> None:
    output = (
        "\x1b[1m\x1b[33mwarning\x1b[0m\x1b[1m: raw wait\x1b[0m\n"
        "   \x1b[1m\x1b[94m--> \x1b[0mcrates/a/src/x.rs:1:2\n"
    )
    assert lint.parse_feature_findings(output) == {("crates/a/src/x.rs", "raw wait"): 1}


def test_product_wait_lint_is_not_capped(monkeypatch, tmp_path) -> None:
    baseline = tmp_path / "baseline.txt"
    baseline.write_text("", encoding="utf-8")
    commands = []

    def fake_run(command, **kwargs):
        commands.append((command, kwargs["env"]))
        return subprocess.CompletedProcess(command, 0, stdout="")

    monkeypatch.setattr(lint, "DYLINT_FEATURE_BASELINE", baseline)
    monkeypatch.setattr(lint.subprocess, "run", fake_run)
    assert lint.dylint_product_pass({}, [None, "x86_64-pc-windows-msvc"]) == 0
    strict = [entry for entry in commands if "dylints/ban_unbounded_sync" in entry[0]]
    assert len(strict) == 2
    for command, env in strict:
        assert "--features" in command
        assert "--cap-lints=warn" not in env.get("DYLINT_RUSTFLAGS", "")


def test_clippy_only_entrypoint_runs_both_feature_selections(monkeypatch) -> None:
    commands = []

    def capture(command):
        commands.append(command)
        return subprocess.CompletedProcess(command, 0)

    monkeypatch.setattr(lint.sys, "argv", ["lint", "--clippy-only"])
    monkeypatch.setattr(lint, "validate_release_metadata", lambda: None)
    monkeypatch.setattr(lint.check_kernal_api_baseline, "check", lambda: [])
    monkeypatch.setattr(lint, "lint_workspace", lambda: 99)
    monkeypatch.setattr(lint, "run_cmd", capture)
    assert lint.main() == 0
    assert len(commands) == 2
    assert all("clippy" in command and "--all-targets" in command for command in commands)
    assert "--features" not in commands[0]
    assert ",".join(lint.DYLINT_PRODUCT_FEATURES) in commands[1]
