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
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys

# Preinstalled toolsets on the GitHub ubuntu image that no zccache lane uses.
UNUSED_SDK_DIRS: tuple[str, ...] = (
    "/usr/local/lib/android",
    "/usr/share/dotnet",
    "/opt/ghc",
    "/usr/local/.ghcup",
    "/opt/hostedtoolcache/CodeQL",
    "/usr/share/swift",
)
DELETE_TIMEOUT_S = 180
GIB = 1024**3


def is_hosted_linux_runner(env: dict[str, str], platform: str = sys.platform) -> bool:
    return (
        platform.startswith("linux")
        and env.get("GITHUB_ACTIONS") == "true"
        and env.get("RUNNER_ENVIRONMENT") == "github-hosted"
    )


def reclaim(dirs: tuple[str, ...], timeout_s: int) -> list[str]:
    present = [d for d in dirs if os.path.isdir(d)]
    if present:
        subprocess.run(["sudo", "rm", "-rf", *present], check=True, timeout=timeout_s)
    return present


def main() -> int:
    if not is_hosted_linux_runner(dict(os.environ)):
        print("reclaim_runner_disk: not a GitHub-hosted Linux runner; skipping")
        return 0
    root = os.environ.get("GITHUB_WORKSPACE", "/")
    before = shutil.disk_usage(root).free / GIB
    removed = reclaim(UNUSED_SDK_DIRS, DELETE_TIMEOUT_S)
    after = shutil.disk_usage(root).free / GIB
    print(f"reclaim_runner_disk: removed {removed}")
    print(f"reclaim_runner_disk: free {before:.2f} GiB -> {after:.2f} GiB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
