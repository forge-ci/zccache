"""Aggregate required check for CI (`CI summary`).

Reads the full job list of this run attempt and fails if any job other than
the exempt ones ended unsuccessfully. Decides from the API rather than
``needs.*.result`` because a reusable-workflow job can end ``cancelled``
while its caller reports success. Standard library only.

Usage: python3 -m ci.ci_summary [--repo OWNER/NAME --run-id N --attempt N]
Defaults come from GITHUB_REPOSITORY / GITHUB_RUN_ID / GITHUB_RUN_ATTEMPT.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request

BLOCKING = frozenset(
    {"failure", "cancelled", "timed_out", "startup_failure", "action_required"}
)
# Only the running aggregate itself is outside the verdict.
EXEMPT_PREFIXES = ("CI summary",)


def blocking_jobs(jobs: list[dict]) -> list[tuple[str, str]]:
    return [
        (j["name"], j["conclusion"])
        for j in jobs
        if j.get("conclusion") in BLOCKING
        and not j["name"].startswith(EXEMPT_PREFIXES)
    ]


def fetch_jobs(repo: str, run_id: str, attempt: str, token: str) -> list[dict]:
    jobs: list[dict] = []
    page = 1
    while True:
        url = (
            f"https://api.github.com/repos/{repo}/actions/runs/{run_id}"
            f"/attempts/{attempt}/jobs?per_page=100&page={page}"
        )
        data = _get(url, token)
        jobs.extend(data["jobs"])
        if len(jobs) >= data["total_count"] or not data["jobs"]:
            return jobs
        page += 1


def _get(url: str, token: str, tries: int = 6) -> dict:
    req = urllib.request.Request(url)
    req.add_header("Accept", "application/vnd.github+json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    delay = 5.0
    for i in range(tries):
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                return json.load(resp)
        except (urllib.error.URLError, TimeoutError) as exc:
            if i == tries - 1:
                raise
            print(f"job-list call failed ({exc}); retry in {delay:.0f}s", flush=True)
            time.sleep(delay)
            delay *= 2
    raise AssertionError("unreachable")


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", ""))
    p.add_argument("--run-id", default=os.environ.get("GITHUB_RUN_ID", ""))
    p.add_argument("--attempt", default=os.environ.get("GITHUB_RUN_ATTEMPT", "1"))
    a = p.parse_args(argv)
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN", "")
    jobs = fetch_jobs(a.repo, a.run_id, a.attempt, token)
    for j in jobs:
        print(f"  {j.get('conclusion') or j.get('status'):<16} {j['name']}")
    blocked = blocking_jobs(jobs)
    if blocked:
        print(f"\nCI summary: {len(blocked)} job(s) blocked the gate:")
        for name, concl in blocked:
            print(f"  BLOCKED {concl}: {name}")
        return 1
    print("\nCI summary: all jobs succeeded or were skipped.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
