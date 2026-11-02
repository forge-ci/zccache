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
            job("Cache janitor", "failure"),
            job("Cancel superseded queue runs", "cancelled"),
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
            if key != "ci-summary" and not job["name"].startswith(EXEMPT_PREFIXES)
        }
        summary = jobs["ci-summary"]
        self.assertEqual(set(summary["needs"]), gated)
        self.assertEqual(summary["name"], "CI summary")
        self.assertEqual(summary["if"], "${{ always() }}")


if __name__ == "__main__":
    unittest.main()
