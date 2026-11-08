import pathlib
import unittest

import yaml

from ci.main_red_notify import SELF_PREFIX, classify, decide, run

GATED = ["ci-macos", "ci-windows", "integration", "wrapper-e2e", "fs-matrix",
         "coverage", "perf-guard", "broker-stress", "test-action", "bench-action"]


def job(name, conclusion):
    return {"name": name, "conclusion": conclusion}


class ClassifyTest(unittest.TestCase):
    def test_code_ops_and_self(self):
        jobs = [job("macos / Test", "failure"), job("Observe macOS runner queue", "failure"),
                job("Cache pre-prune", "timed_out"), job("x86", "cancelled"), job(SELF_PREFIX, None),
                job("x86", "success"), job("arm", "skipped")]
        code, ops = classify(jobs)
        self.assertEqual(code, ["macos / Test (failure)"])
        self.assertEqual(len(ops), 2)


class DecideTest(unittest.TestCase):
    def test_matrix(self):
        self.assertEqual(decide(["a"], [], False), "open")
        self.assertEqual(decide(["a"], [], True), "comment")
        self.assertEqual(decide([], [], True), "close")
        self.assertEqual(decide([], [], False), "none")
        # ops-only failure neither opens nor closes a code-gate issue
        self.assertEqual(decide([], ["o"], True), "none")
        self.assertEqual(decide([], ["o"], False), "none")

    def test_non_main_and_dispatch_are_ignored(self):
        base = {"GITHUB_REPOSITORY": "o/r"}
        self.assertTrue(run({**base, "GITHUB_REF": "refs/heads/x", "GITHUB_EVENT_NAME": "push"}).startswith("none"))
        self.assertTrue(run({**base, "GITHUB_REF": "refs/heads/main", "GITHUB_EVENT_NAME": "workflow_dispatch"}).startswith("none"))


class WiringTest(unittest.TestCase):
    """Every push-only gate ends with the notifier, which waits for every job."""

    def test_each_gate_wires_notifier(self):
        root = pathlib.Path(__file__).resolve().parents[2] / ".github/workflows"
        for wf in GATED:
            jobs = yaml.safe_load((root / f"{wf}.yml").read_text())["jobs"]
            n = jobs.get("main-red-notify")
            self.assertIsNotNone(n, wf)
            self.assertEqual(n["name"], SELF_PREFIX, wf)
            self.assertEqual(set(n["needs"]), set(jobs) - {"main-red-notify"}, wf)
            self.assertIn("always()", n["if"], wf)
            self.assertEqual(n["permissions"].get("issues"), "write", wf)
