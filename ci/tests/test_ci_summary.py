import pathlib
import unittest

import pytest
import yaml

from ci import ci_summary
from ci.ci_summary import EXEMPT_PREFIXES, blocking_jobs


def job(name, conclusion):
    return {"name": name, "conclusion": conclusion}


@pytest.mark.parametrize("name", ["Dylint", "Dylint (product features)"])
def test_dylint_runtime_guard_blocks_a_green_job_before_timeout(monkeypatch, capsys, name):
    row = {
        **job(name, "success"),
        "started_at": "2026-11-13T03:00:00Z",
        "completed_at": "2026-11-13T03:20:01Z",
    }
    monkeypatch.setattr(ci_summary, "fetch_jobs", lambda *args: [row])
    assert ci_summary.main([]) == 1
    output = capsys.readouterr().out
    assert name in output and "runtime" in output and "20m" in output


@pytest.mark.parametrize("completed", ["2026-11-13T03:00:01Z", "2026-11-13T03:20:00Z"])
def test_dylint_runtime_guard_accepts_up_to_eighty_percent(completed):
    row = {
        **job("Dylint", "success"),
        "started_at": "2026-11-13T03:00:00Z",
        "completed_at": completed,
    }
    assert ci_summary.runtime_violations([row]) == []


@pytest.mark.parametrize("field,value", [
    ("started_at", None), ("completed_at", "invalid"),
    ("completed_at", "2026-11-13T02:59:59Z"),
    ("started_at", "2026-11-13T03:00:00"),
])
def test_dylint_runtime_guard_rejects_missing_or_invalid_evidence(field, value):
    row = {
        **job("Dylint", "success"),
        "started_at": "2026-11-13T03:00:00Z",
        "completed_at": "2026-11-13T03:01:00Z",
        field: value,
    }
    assert ci_summary.runtime_violations([row])


def test_dylint_runtime_guard_ignores_skips_and_other_jobs():
    assert ci_summary.runtime_violations([
        job("Dylint", "skipped"), job("Dylint (product features)", "skipped"),
        job("Integration (Linux)", "success"),
    ]) == []


def test_dylint_runtime_guard_tracks_the_workflow_budget():
    wf = pathlib.Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml"
    jobs = yaml.safe_load(wf.read_text())["jobs"]
    dylint = {j["name"]: j for key, j in jobs.items() if key.startswith("dylint")}
    assert set(dylint) == ci_summary.DYLINT_JOBS
    assert all(j["timeout-minutes"] == 25 for j in dylint.values())
    assert ci_summary.DYLINT_RUNTIME_BUDGET_NS == 25 * 60 * 1_000_000_000 * 80 // 100


class BlockingJobsTest(unittest.TestCase):
    def test_green_and_skipped_pass(self):
        jobs = [job("Dylint", "success"), job("Formatting", "skipped")]
        self.assertEqual(blocking_jobs(jobs), [])

    def test_each_blocking_conclusion_fails(self):
        name = "Reflink e2e / Reflink e2e (btrfs)"
        for c in ("failure", "cancelled", "timed_out", "startup_failure", "action_required"):
            self.assertEqual(blocking_jobs([job(name, c)]), [(name, c)])

    def test_exempt_and_in_progress_ignored(self):
        jobs = [
            job("CI summary", None),
            job("Cache janitor", "success"),
            job("Cancel superseded queue runs", "success"),
            job("MSRV (1.95.0)", None),
        ]
        self.assertEqual(blocking_jobs(jobs), [])


class SummaryNeedsEveryJobTest(unittest.TestCase):
    """The gate must wait for every non-maintenance job, or it can pass early."""

    def test_needs_lists_every_gated_job(self):
        wf = pathlib.Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml"
        jobs = yaml.safe_load(wf.read_text())["jobs"]
        gated = {
            key
            for key, job in jobs.items()
            if key != "ci-summary" and not job.get("name", "").startswith(EXEMPT_PREFIXES)
        }
        summary = jobs["ci-summary"]
        self.assertEqual(set(summary["needs"]), gated)
        self.assertEqual(summary["name"], "CI summary")
        self.assertEqual(summary["if"], "${{ always() }}")


class WorkflowOwnershipTest(unittest.TestCase):
    def test_all_pr_checks_belong_to_ci(self):
        root = pathlib.Path(__file__).resolve().parents[2]
        workflows = root / ".github/workflows"
        ci = yaml.safe_load((workflows / "ci.yml").read_text())
        triggers = ci.get("on", ci.get(True))
        self.assertIn("pull_request", triggers)
        self.assertIn("merge_group", triggers)
        self.assertNotIn("paths-ignore", triggers["pull_request"])
        called = {j.get("uses") for j in ci["jobs"].values()}
        for path in workflows.glob("*.yml"):
            if path.name == "ci.yml":
                continue
            workflow = yaml.safe_load(path.read_text())
            events = workflow.get("on", workflow.get(True, {}))
            self.assertNotIn("pull_request", events, path.name)
            if path.name in {"ci-linux.yml", "python-tests.yml",
                             "wire-stability.yml", "feature-matrix-check.yml"}:
                self.assertIn("workflow_call", events, path.name)
                self.assertIn(f"./.github/workflows/{path.name}", called)

    def test_maintenance_failures_also_block(self):
        self.assertEqual(blocking_jobs([job("Cache janitor", "failure")]),
                         [("Cache janitor", "failure")])


if __name__ == "__main__":
    unittest.main()
