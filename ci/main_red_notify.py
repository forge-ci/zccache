"""Main-red notifier for gates that run only on push/schedule (not pre-merge).

The merge queue's required check (`CI summary`, ci/ci_summary.py) only sees
jobs in ci.yml's own run. Hosted macOS/Windows, coverage, perf, e2e and
filesystem-matrix gates are too slow for the queue's 60-minute
check_response_timeout, so they stay push/schedule-only. Without this script
a deterministic defect they catch reddens main and nobody is told.

Run as the last job of such a workflow. It reads this run attempt's job list
(same API read and blocking set as ci_summary) and keeps one tracking issue per
workflow, labelled ``main-red``:

* a code gate failed  -> open the issue, or comment on the open one;
* everything passed   -> close the open issue with the green run linked;
* only ops jobs failed (runner-queue observers, cache pre-prune) -> no issue:
  those are infrastructure signals, not code defects.

It never fails its own job: a notifier outage must not redden main either.
Only main pushes and scheduled runs on main count; dispatch runs are manual.

Usage: uv run --no-project --python 3.13 python -m ci.main_red_notify
Environment: GITHUB_REPOSITORY, GITHUB_RUN_ID, GITHUB_RUN_ATTEMPT,
GITHUB_WORKFLOW, GITHUB_REF, GITHUB_EVENT_NAME, GITHUB_SHA, GH_TOKEN.
"""

from __future__ import annotations

import json
import os
import sys
import urllib.parse
import urllib.request

from ci.ci_summary import BLOCKING, fetch_jobs

LABEL = "main-red"
SELF_PREFIX = "Main red notifier"
# Infrastructure jobs: their failure is not a code defect on main.
OPS_PREFIXES = ("Observe ", "Cache pre-prune", "cache-pre-prune")
COUNTED_EVENTS = frozenset({"push", "schedule"})
# A cancelled job (superseded main push, manual cancel) is not a code defect;
# counting it would open an issue for every overtaken run.
NOT_A_DEFECT = frozenset({"cancelled"})


def classify(jobs: list[dict]) -> tuple[list[str], list[str]]:
    """Split blocking jobs into (code-gate failures, ops failures)."""
    code: list[str] = []
    ops: list[str] = []
    for j in jobs:
        name = j["name"]
        if name.startswith(SELF_PREFIX) or j.get("conclusion") not in BLOCKING:
            continue
        if j["conclusion"] in NOT_A_DEFECT:
            continue
        label = f"{name} ({j['conclusion']})"
        (ops if name.startswith(OPS_PREFIXES) else code).append(label)
    return code, ops


def decide(code: list[str], ops: list[str], issue_open: bool) -> str:
    """Return one of: open, comment, close, none."""
    if code:
        return "comment" if issue_open else "open"
    if not ops and issue_open:
        return "close"
    return "none"


def issue_title(workflow: str) -> str:
    return f"main red: {workflow}"


def _api(method: str, url: str, token: str, body: dict | None = None) -> object:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Accept", "application/vnd.github+json")
    req.add_header("Authorization", f"Bearer {token}")
    if data is not None:
        req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=30) as resp:
        raw = resp.read()
    return json.loads(raw) if raw else None


def find_open_issue(repo: str, title: str, token: str) -> int | None:
    q = urllib.parse.urlencode({"labels": LABEL, "state": "open", "per_page": 100})
    issues = _api("GET", f"https://api.github.com/repos/{repo}/issues?{q}", token)
    for issue in issues or []:  # type: ignore[union-attr]
        if issue.get("title") == title and "pull_request" not in issue:
            return int(issue["number"])
    return None


def run(env: dict[str, str]) -> str:
    repo = env["GITHUB_REPOSITORY"]
    workflow = env.get("GITHUB_WORKFLOW", "workflow")
    if env.get("GITHUB_REF") != "refs/heads/main" or env.get(
        "GITHUB_EVENT_NAME"
    ) not in COUNTED_EVENTS:
        return "none (not a main push/schedule run)"
    token = env.get("GH_TOKEN") or env.get("GITHUB_TOKEN", "")
    run_id = env["GITHUB_RUN_ID"]
    jobs = fetch_jobs(repo, run_id, env.get("GITHUB_RUN_ATTEMPT", "1"), token)
    code, ops = classify(jobs)
    title = issue_title(workflow)
    number = find_open_issue(repo, title, token)
    action = decide(code, ops, number is not None)
    run_url = f"https://github.com/{repo}/actions/runs/{run_id}"
    sha = env.get("GITHUB_SHA", "")[:10]
    base = f"https://api.github.com/repos/{repo}/issues"
    lines = "\n".join(f"- {c}" for c in code)
    if action == "open":
        body = (
            f"`{workflow}` failed on main at {sha} ({env['GITHUB_EVENT_NAME']}).\n\n"
            f"Run: {run_url}\n\nFailed code-gate jobs:\n{lines}\n\n"
            "This gate is push/schedule-only (too slow for the merge queue), so "
            "the queue could not catch it. This issue closes itself when the "
            "workflow is green on main again (ci/main_red_notify.py)."
        )
        _api("POST", base, token, {"title": title, "body": body, "labels": [LABEL]})
    elif action == "comment":
        _api(
            "POST",
            f"{base}/{number}/comments",
            token,
            {"body": f"Still red at {sha}: {run_url}\n\n{lines}"},
        )
    elif action == "close":
        _api(
            "POST",
            f"{base}/{number}/comments",
            token,
            {"body": f"Green again at {sha}: {run_url}"},
        )
        _api("PATCH", f"{base}/{number}", token, {"state": "closed"})
    if ops and not code:
        print(f"ops-only failures (no code-gate issue): {ops}")
    return action


def main() -> int:
    try:
        print(f"main-red notifier: {run(dict(os.environ))}")
    except Exception as exc:  # never redden main from the notifier itself
        print(f"::warning::main-red notifier failed: {exc}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
