"""Reclaim disk on GitHub-hosted Linux runners before a large Rust build.

ubuntu-latest ships ~14 GiB free on ``/``. After setup-soldr restores the
toolchain, registry and target caches, the ``--release`` native Python
artifact build dropped the build volume to 4.63 GiB and tripped soldr's
5 GiB target block guard (main run 41507829214). The guard is correct; the
lane was starving it. This helper deletes preinstalled SDKs no zccache lane
uses (Android, .NET, GHC, CodeQL, Swift) and reports free space before and
after.

It is a no-op off GitHub-hosted Linux runners, so it never touches a
developer machine or a self-hosted runner.

Reclaim is best-effort: a slow or failed delete logs a ``::warning::`` and the
script still exits 0. Soldr's block guard is the real protection and still
fails fast if the disk really is too low, so a slow runner must not fail a job
here (run 42197407565 lost its merge-queue slot to a 180 s ``sudo rm`` timeout).
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import time

# Preinstalled toolsets on the GitHub ubuntu image that no zccache lane uses.
UNUSED_SDK_DIRS: tuple[str, ...] = (
    "/usr/local/lib/android",
    "/usr/share/dotnet",
    "/opt/ghc",
    "/usr/local/.ghcup",
    "/opt/hostedtoolcache/CodeQL",
    "/usr/share/swift",
)
DELETE_COMMAND: tuple[str, ...] = ("sudo", "rm", "-rf")
# One slow directory must not use up the time the others need, and the step's
# overall wait stays at the original bound however many directories are slow.
DIR_TIMEOUT_S = 60
TOTAL_BUDGET_S = 180
GIB = 1024**3


def is_hosted_linux_runner(env: dict[str, str], platform: str = sys.platform) -> bool:
    return (
        platform.startswith("linux")
        and env.get("GITHUB_ACTIONS") == "true"
        and env.get("RUNNER_ENVIRONMENT") == "github-hosted"
    )


def warn(message: str) -> None:
    print(f"::warning::reclaim_runner_disk: {message}", flush=True)


def reclaim(
    dirs: tuple[str, ...],
    dir_timeout_s: float,
    total_budget_s: float,
    command: tuple[str, ...],
) -> list[str]:
    """Delete each present directory, warn on any problem, return those removed."""
    removed: list[str] = []
    deadline = time.monotonic() + total_budget_s
    for path in dirs:
        if not os.path.isdir(path):
            continue
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            warn(f"skipped {path}: {total_budget_s:g}s total budget used up")
            continue
        timeout_s = min(dir_timeout_s, remaining)
        try:
            # On timeout this SIGKILLs sudo, which cannot forward the signal, so
            # rm may keep running and freeing space in the background. That is
            # acceptable on an ephemeral runner. Stdio stays inherited because
            # captured pipes held open by that rm would block subprocess.run.
            result = subprocess.run([*command, path], timeout=timeout_s, check=False)
        except subprocess.TimeoutExpired:
            warn(f"deleting {path} timed out after {timeout_s:g}s; continuing")
        except OSError as exc:
            warn(f"could not run delete for {path}: {exc}; continuing")
        else:
            if result.returncode == 0:
                removed.append(path)
            else:
                warn(f"deleting {path} exited {result.returncode}; continuing")
    return removed


def main() -> int:
    if not is_hosted_linux_runner(dict(os.environ)):
        print("reclaim_runner_disk: not a GitHub-hosted Linux runner; skipping")
        return 0
    root = os.environ.get("GITHUB_WORKSPACE", "/")
    before = shutil.disk_usage(root).free / GIB
    removed = reclaim(UNUSED_SDK_DIRS, DIR_TIMEOUT_S, TOTAL_BUDGET_S, DELETE_COMMAND)
    after = shutil.disk_usage(root).free / GIB
    print(f"reclaim_runner_disk: removed {removed}")
    print(f"reclaim_runner_disk: free {before:.2f} GiB -> {after:.2f} GiB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
