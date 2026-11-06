import pathlib
import unittest

import yaml

from ci.ci_summary import EXEMPT_PREFIXES, blocking_jobs


def job(name, conclusion):
    return {"name": name, "conclusion": conclusion}


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
