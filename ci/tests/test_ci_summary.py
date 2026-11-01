import unittest

from ci.ci_summary import blocking_jobs


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


if __name__ == "__main__":
    unittest.main()
