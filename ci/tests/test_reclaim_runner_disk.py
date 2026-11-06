"""ci/reclaim_runner_disk.py must free what it can and never fail the job."""

import subprocess
import sys
from pathlib import Path

import pytest

from ci import reclaim_runner_disk as reclaim_mod

# Fake delete command: `<python> -c FAKE <dir>`. Behaviour follows the dir name.
FAKE_DELETE = """
import shutil, sys, time
name = sys.argv[1].replace("\\\\", "/").rsplit("/", 1)[-1]
if name == "slow":
    time.sleep(120)
if name == "bad":
    sys.exit(3)
shutil.rmtree(sys.argv[1])
"""
FAKE_COMMAND = (sys.executable, "-c", FAKE_DELETE)
GENEROUS_S = 120.0


def make_dirs(root: Path, *names: str) -> tuple[str, ...]:
    paths = [root / name for name in names]
    for path in paths:
        path.mkdir()
    return tuple(str(path) for path in paths)


def test_command_that_times_out_warns_and_does_not_raise(tmp_path, capsys):
    dirs = make_dirs(tmp_path, "slow")

    assert reclaim_mod.reclaim(dirs, 0.2, GENEROUS_S, FAKE_COMMAND) == []

    out = capsys.readouterr().out
    assert f"::warning::reclaim_runner_disk: deleting {dirs[0]} timed out" in out


def test_command_that_fails_warns_and_later_dirs_are_still_removed(tmp_path, capsys):
    bad, good = make_dirs(tmp_path, "bad", "good")

    removed = reclaim_mod.reclaim((bad, good), GENEROUS_S, GENEROUS_S, FAKE_COMMAND)

    assert removed == [good]
    assert not Path(good).exists()
    assert f"::warning::reclaim_runner_disk: deleting {bad} exited 3" in capsys.readouterr().out


def test_timeout_on_one_dir_still_deletes_the_next(tmp_path, monkeypatch):
    slow, good = make_dirs(tmp_path, "slow", "good")
    real_run = subprocess.run

    # Patched rather than timed: a real short timeout could also expire on the
    # good directory when the runner is loaded.
    def run(args, **kwargs):
        if args[-1] == slow:
            raise subprocess.TimeoutExpired(args, kwargs["timeout"])
        return real_run(args, **kwargs)

    monkeypatch.setattr(reclaim_mod.subprocess, "run", run)

    assert reclaim_mod.reclaim((slow, good), 60, GENEROUS_S, FAKE_COMMAND) == [good]
    assert not Path(good).exists()


def test_missing_delete_command_warns_and_does_not_raise(tmp_path, capsys):
    dirs = make_dirs(tmp_path, "any")
    command = (str(tmp_path / "no-such-binary"),)

    assert reclaim_mod.reclaim(dirs, GENEROUS_S, GENEROUS_S, command) == []
    assert "::warning::reclaim_runner_disk: could not run delete" in capsys.readouterr().out


def test_spent_budget_skips_remaining_dirs(tmp_path, capsys):
    dirs = make_dirs(tmp_path, "good")

    assert reclaim_mod.reclaim(dirs, GENEROUS_S, 0, FAKE_COMMAND) == []

    assert Path(dirs[0]).exists()
    assert f"skipped {dirs[0]}: 0s total budget used up" in capsys.readouterr().out


def test_absent_dirs_are_not_attempted(tmp_path, capsys):
    command = (str(tmp_path / "would-warn-if-run"),)

    assert reclaim_mod.reclaim((str(tmp_path / "gone"),), 1, 1, command) == []
    assert capsys.readouterr().out == ""


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="hosted Linux path")
def test_main_exits_zero_and_reports_free_space_on_timeout_and_failure(
    tmp_path, monkeypatch, capsys
):
    dirs = make_dirs(tmp_path, "slow", "bad")
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    monkeypatch.setenv("RUNNER_ENVIRONMENT", "github-hosted")
    monkeypatch.setenv("GITHUB_WORKSPACE", str(tmp_path))
    monkeypatch.setattr(reclaim_mod, "UNUSED_SDK_DIRS", dirs)
    monkeypatch.setattr(reclaim_mod, "DELETE_COMMAND", FAKE_COMMAND)
    monkeypatch.setattr(reclaim_mod, "DIR_TIMEOUT_S", 0.2)
    monkeypatch.setattr(reclaim_mod, "TOTAL_BUDGET_S", GENEROUS_S)

    assert reclaim_mod.main() == 0

    out = capsys.readouterr().out
    assert out.count("::warning::") == 2
    assert "reclaim_runner_disk: free " in out


def test_main_is_a_noop_off_hosted_runners(monkeypatch, capsys):
    def reclaim(*_args, **_kwargs):
        raise AssertionError("must not delete off a hosted runner")

    monkeypatch.setattr(reclaim_mod, "reclaim", reclaim)
    monkeypatch.setenv("GITHUB_ACTIONS", "true")
    monkeypatch.setenv("RUNNER_ENVIRONMENT", "self-hosted")

    assert reclaim_mod.main() == 0
    assert "skipping" in capsys.readouterr().out
