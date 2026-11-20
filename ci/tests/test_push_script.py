"""Exercise the root publishing command without pushing to GitHub."""

import os
import runpy
import subprocess
from pathlib import Path

import pytest

from ci import local_gate

PUSH = Path(__file__).resolve().parents[2] / "push"


@pytest.fixture
def push(monkeypatch):
    # run_path's returned dict is a copy of the script's globals.
    monkeypatch.syspath_prepend(str(PUSH.parent / "ci"))
    return runpy.run_path(str(PUSH))["main"].__globals__


@pytest.fixture
def checkout(tmp_path, monkeypatch, push):
    monkeypatch.setitem(push, "ROOT", tmp_path)

    def git(*args):
        subprocess.run(["git", *args], cwd=tmp_path, check=True)

    tracked = tmp_path / "tracked"
    git("init", "-q", "-b", "main")
    tracked.write_text("committed\n")
    git("add", "tracked")
    git("-c", "user.name=Push Test", "-c", "user.email=push@example.invalid",
        "commit", "-qm", "Initial commit")
    return tmp_path, git


def test_push_uses_the_ci_lint_pin(push):
    assert os.access(PUSH, os.X_OK)
    assert f"git+https://github.com/zackees/ci.yml@{local_gate.CI_LINT_REF}" in push["CI_LINT"]


def test_refuses_default_detached_and_tracked_changes(push, checkout, monkeypatch):
    root, git = checkout

    def unexpected_step(argv):
        pytest.fail(f"ran a command after refusal: {argv}")

    monkeypatch.setitem(push, "step", unexpected_step)
    assert push["main"]([]) == 3
    git("switch", "-q", "--detach")
    assert push["main"]([]) == 3
    git("switch", "-qc", "topic")
    git("symbolic-ref", "refs/remotes/mirror/HEAD", "refs/remotes/mirror/topic")
    assert push["main"](["--remote", "mirror"]) == 3
    (root / "tracked").write_text("modified\n")
    assert push["main"]([]) == 3
    git("add", "tracked")
    assert push["main"]([]) == 3


@pytest.mark.parametrize(
    ("flags", "open_prs", "publish"),
    [
        ([], "0", ["push", "list", "create"]),
        ([], "1", ["push", "list"]),
        (["--no-pr"], "0", ["push"]),
        (["--prove-only"], "0", []),
    ],
)
def test_attest_and_verify_before_any_publication(
    flags, open_prs, publish, push, checkout, monkeypatch
):
    root, git = checkout
    git("switch", "-qc", "topic")
    (root / "untracked").touch()
    commands = []
    original_read = push["read_command"]

    def read(*argv, **kwargs):
        if argv[:2] == ("gh", "pr"):
            commands.append(list(argv))
            return open_prs
        return original_read(*argv, **kwargs)

    monkeypatch.setitem(push, "read_command", read)
    def step(argv, *, env=None):
        commands.append(argv)
        if env:
            Path(env["GITHUB_OUTPUT"]).write_text("attested=true\n")

    monkeypatch.setitem(push, "step", step)
    assert push["main"](["--remote", "mirror", *flags]) == 0
    pin = push["CI_LINT"]
    head = original_read("git", "rev-parse", "HEAD")
    assert commands[:2] == [
        [*pin, "local-gate", "run"],
        [*pin, "local-gate", "verify", "--repo", ".", "--sha", head,
         "--base-sha", head, "--event", "pull_request", "--github-output"],
    ]
    requested = {
        "push": ["git", "push", "--force-with-lease", "-u", "--", "mirror", "HEAD:refs/heads/topic"],
        "list": ["gh", "pr", "list", "--head", "topic", "--state", "open", "--json", "number", "--jq", "length"],
        "create": ["gh", "pr", "create", "--fill", "--head", "topic"],
    }
    assert commands[2:] == [requested[action] for action in publish]


@pytest.mark.parametrize("failure", ["run", "verify"])
def test_gate_failure_prevents_push(failure, push, checkout, monkeypatch):
    _, git = checkout
    git("switch", "-qc", "topic")
    calls = []

    def step(argv, *, env=None):
        calls.append(argv)
        if failure in argv:
            raise subprocess.CalledProcessError(17, argv)

    monkeypatch.setitem(push, "step", step)
    assert push["main"]([]) == 1
    actions = ["run"] if failure == "run" else ["run", "verify"]
    assert [argv[len(push["CI_LINT"]) + 1] for argv in calls] == actions


def test_rejects_a_successful_verifier_with_no_attestation(push, checkout, monkeypatch):
    _, git = checkout
    git("switch", "-qc", "topic")
    calls = []

    def step(argv, *, env=None):
        calls.append(argv)
        if env:
            Path(env["GITHUB_OUTPUT"]).write_text("attested=false\nstate=missing\n")

    monkeypatch.setitem(push, "step", step)
    assert push["main"]([]) == 1
    assert len(calls) == 2


def test_no_attestation_bypass_flag(push):
    with pytest.raises(SystemExit) as error:
        push["main"](["--no-proof"])
    assert error.value.code == 2
