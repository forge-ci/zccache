"""Cancel merge-queue runs that a newer queue run for the same PR replaced (#2048).

    python -m ci.cancel_superseded [--dry-run]

Runs as a job of every `merge_group` CI run (GITHUB_RUN_ID, GITHUB_REPOSITORY
and a GH_TOKEN with actions: write). A queue ref carries the base commit,
`gh-readonly-queue/main/pr-<N>-<sha>`, so the workflow's concurrency group
never pairs a PR's rebuilt queue run with its old one. When main moves,
GitHub starts a new run per queued PR and the old ones keep holding runners.
The newest queue run for a PR is the live one, so every older in-flight
queue run for the same PR is cancelled. Ported from obs-rust/obs-studio's
`ci/cancel_superseded.py` (#986 there).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from urllib.error import HTTPError
from urllib.request import Request, urlopen

QUEUE_RE = re.compile(r"(?:^|/)gh-readonly-queue/[^/]+/pr-(\d+)-[0-9a-f]+$")
LIVE = ("queued", "in_progress", "waiting", "pending", "requested")


def queue_pr(ref: str) -> int | None:
    """The PR number of a merge-queue ref, else None."""
    match = QUEUE_RE.search(ref)
    return int(match.group(1)) if match else None


def superseded(runs: list[dict], current: dict) -> list[int]:
    """Ids of in-flight queue runs for current's PR that started before it."""
    pr = queue_pr(current.get("head_branch") or "")
    if pr is None:
        return []
    return sorted(
        {
            run["id"]
            for run in runs
            if run["id"] != current["id"]
            and run.get("status") in LIVE
            and queue_pr(run.get("head_branch") or "") == pr
            and run["created_at"] < current["created_at"]
        }
    )


def api(path: str, method: str = "GET") -> dict:
    request = Request(
        f"https://api.github.com/{path}",
        method=method,
        headers={
            "Authorization": f"Bearer {os.environ['GH_TOKEN']}",
            "Accept": "application/vnd.github+json",
        },
    )
    with urlopen(request, timeout=30) as response:
        body = response.read()
    return json.loads(body) if body else {}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)
    repo = os.environ["GITHUB_REPOSITORY"]
    current = api(f"repos/{repo}/actions/runs/{os.environ['GITHUB_RUN_ID']}")
    workflow = current["workflow_id"]
    runs: list[dict] = []
    for status in LIVE:
        listing = api(
            f"repos/{repo}/actions/workflows/{workflow}/runs"
            f"?event=merge_group&status={status}&per_page=100"
        )
        runs += listing["workflow_runs"]
    stale = superseded(runs, current)
    pr = queue_pr(current.get("head_branch") or "")
    print(f"PR #{pr}: cancelling {len(stale)} superseded queue run(s): {stale}")
    for run_id in stale:
        if args.dry_run:
            continue
        try:
            api(f"repos/{repo}/actions/runs/{run_id}/cancel", method="POST")
        except HTTPError as error:
            # A run that finished meanwhile can no longer be cancelled (409).
            print(f"run {run_id}: cancel returned {error.code}; leaving it")
    return 0


if __name__ == "__main__":
    sys.exit(main())
