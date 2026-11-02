"""Regression coverage for bounded hosted-runner queue diagnostics (#1541)."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest
from ci import observe_runner_queue

NOW = datetime(2026, 8, 28, 6, 0, tzinfo=UTC)
RUN = {"created_at": "2026-08-28T05:30:00Z"}


def _job(**overrides: object) -> dict[str, object]:
    job: dict[str, object] = {
        "name": "matrix (macos-15)",
        "status": "queued",
        "conclusion": None,
        "created_at": "2026-08-28T05:30:00Z",
        "started_at": None,
        "completed_at": None,
        "labels": ["macos-15"],
    }
    job.update(overrides)
    return job


def test_queued_job_reports_age_and_crosses_the_bound() -> None:
    observation = observe_runner_queue.observe_once(
        RUN,
        [_job()],
        ["matrix (macos-15)"],
        now=NOW,
        configured_runner="macos-15",
    )[0]

    assert observation.state is observe_runner_queue.QueueState.QUEUED
    assert observation.queue_age == timedelta(minutes=30)
    assert observe_runner_queue.queue_violations([observation], timedelta(minutes=15)) == [observation]
    summary = observe_runner_queue.render_summary([observation], repository="zackees/zccache", run_id=123)
    assert "Workflow: `zackees/zccache` run `123`" in summary
    assert "| `matrix (macos-15)` | `macos-15` | `queued` |" in summary


def test_started_and_completed_jobs_are_not_queue_failures() -> None:
    observations = observe_runner_queue.observe_once(
        RUN,
        [
            _job(status="in_progress", started_at="2026-08-28T05:35:00Z"),
            _job(
                name="Test PyPI Wheel (macosx_10_12_x86_64)",
                status="completed",
                conclusion="success",
                started_at="2026-08-28T05:34:00Z",
                completed_at="2026-08-28T05:40:00Z",
            ),
        ],
        ["matrix (macos-15)", "Test PyPI Wheel (macosx_10_12_x86_64)"],
        now=NOW,
        configured_runner="macos-15",
    )

    assert [entry.state for entry in observations] == [
        observe_runner_queue.QueueState.STARTED,
        observe_runner_queue.QueueState.COMPLETED,
    ]
    assert observe_runner_queue.queue_violations(observations, timedelta(seconds=1)) == []


def test_immediate_start_is_reported_as_zero_queue_seconds() -> None:
    observation = observe_runner_queue.observe_once(
        RUN,
        [_job(status="in_progress", started_at="2026-08-28T05:30:00Z")],
        ["matrix (macos-15)"],
        now=NOW,
        configured_runner="macos-15",
    )[0]

    assert "| `matrix (macos-15)` | `macos-15` | `started` | 0s |" in observe_runner_queue.render_summary([observation])


def test_missing_job_uses_workflow_queue_age() -> None:
    observation = observe_runner_queue.observe_once(
        RUN,
        [],
        ["wrapper-e2e (macos-15)"],
        now=NOW,
        configured_runner="macos-15",
    )[0]

    assert observation.state is observe_runner_queue.QueueState.MISSING
    assert observation.queue_age is None
    assert observe_runner_queue.queue_violations([observation], timedelta(seconds=1), missing_visible=False) == []
    assert observe_runner_queue.queue_violations([observation], timedelta(seconds=1), missing_visible=True) == [observation]


def test_late_job_visibility_uses_the_target_job_created_at_not_old_run_age() -> None:
    missing = observe_runner_queue.observe_once(
        RUN,
        [],
        ["wrapper-e2e (macos-15)"],
        now=NOW,
        configured_runner="macos-15",
    )[0]
    present = observe_runner_queue.observe_once(
        RUN,
        [
            _job(
                name="wrapper-e2e (macos-15)",
                created_at="2026-08-28T05:59:30Z",
            )
        ],
        ["wrapper-e2e (macos-15)"],
        now=NOW,
        configured_runner="macos-15",
    )[0]

    assert missing.queue_age is None
    assert observe_runner_queue.queue_violations([missing], timedelta(seconds=1), missing_visible=False) == []
    assert present.queue_age == timedelta(seconds=30)
    assert observe_runner_queue.queue_violations([present], timedelta(minutes=1), missing_visible=True) == []


def test_fetches_every_job_page_before_matching_an_expected_job() -> None:
    class PaginatedApi:
        def __init__(self) -> None:
            self.paths: list[str] = []

        def get_json(self, path: str) -> object:
            self.paths.append(path)
            if path.endswith("/runs/123"):
                return RUN
            if path.endswith("jobs?per_page=100&page=1"):
                return {"total_count": 101, "jobs": [_job(name="first page")] * 100}
            if path.endswith("jobs?per_page=100&page=2"):
                return {"total_count": 101, "jobs": [_job(name="matrix (macos-15)")]}
            raise AssertionError(path)

    api = PaginatedApi()
    _run, jobs = observe_runner_queue.fetch_workflow_state(api, "zackees/zccache", 123)

    assert len(jobs) == 101
    assert api.paths[-1].endswith("jobs?per_page=100&page=2")


def test_api_error_is_loud_and_distinct_from_a_queued_job() -> None:
    class BrokenApi:
        def get_json(self, _path: str) -> object:
            raise observe_runner_queue.GitHubApiError("HTTP 503")

    with pytest.raises(observe_runner_queue.GitHubApiError, match="HTTP 503"):
        observe_runner_queue.fetch_workflow_state(BrokenApi(), "zackees/zccache", 123)


def test_missing_job_is_pending_while_an_upstream_job_is_unfinished() -> None:
    jobs = [
        {"name": "Observe macOS runner queue", "runner_name": "GitHub Actions 7", "completed_at": None},
        {"name": "Cache pre-prune barrier / Wait", "runner_name": "GitHub Actions 3", "completed_at": None},
    ]
    assert observe_runner_queue.upstream_pending(jobs, ["macos / Test"], self_runner="GitHub Actions 7")


def test_missing_job_is_a_violation_once_only_the_observer_remains() -> None:
    jobs = [
        {"name": "Observe macOS runner queue", "runner_name": "GitHub Actions 7", "completed_at": None},
        {"name": "Cache pre-prune barrier / Wait", "runner_name": "GitHub Actions 3", "completed_at": "2026-09-29T04:41:00Z"},
    ]
    assert not observe_runner_queue.upstream_pending(jobs, ["macos / Test"], self_runner="GitHub Actions 7")


def test_started_at_without_an_acquired_runner_is_still_queued() -> None:
    # forge-ci/zccache run 41494338424: GitHub stamped `macos / Test` with
    # started_at == created_at while no hosted runner had acquired it
    # (runner_name ""), so the observer exited 0 and the job was later
    # cancelled with "not acquired by Runner of type hosted".
    observation = observe_runner_queue.observe_once(
        RUN,
        [_job(status="queued", started_at="2026-08-28T05:30:00Z", runner_name="")],
        ["matrix (macos-15)"],
        now=NOW,
        configured_runner="macos-15",
    )[0]

    assert observation.state is observe_runner_queue.QueueState.QUEUED
    assert observation.queue_age == timedelta(minutes=30)
    assert observe_runner_queue.queue_violations([observation], timedelta(minutes=15)) == [observation]


# Decision-function fixtures for the wrapper-e2e false-cancel (run 41507829143).
_MAX = timedelta(minutes=15)
V = observe_runner_queue.Verdict


def _decide(job: dict[str, object] | None, *, deadline_expired: bool, missing_visible: bool = False) -> observe_runner_queue.Verdict:
    jobs = [job] if job is not None else []
    observations = observe_runner_queue.observe_once(RUN, jobs, ["matrix (macos-15)"], now=NOW, configured_runner="macos-15")
    return observe_runner_queue.decide(observations, _MAX, missing_visible=missing_visible, deadline_expired=deadline_expired)[0]


def test_never_acquired_is_named_starvation() -> None:
    assert _decide(_job(), deadline_expired=False) is V.STARVED


def test_acquired_and_passed_exits_clean_even_at_deadline() -> None:
    job = _job(status="completed", conclusion="success", started_at="2026-08-28T05:31:00Z", completed_at="2026-08-28T05:50:00Z")
    assert _decide(job, deadline_expired=True) is V.PASS


def test_acquired_and_failed_is_not_the_observers_failure() -> None:
    job = _job(status="completed", conclusion="failure", started_at="2026-08-28T05:31:00Z", completed_at="2026-08-28T05:50:00Z")
    assert _decide(job, deadline_expired=True) is V.PASS


def test_still_queued_at_deadline_is_a_named_failure() -> None:
    recent = _job(created_at="2026-08-28T05:55:00Z")
    assert _decide(recent, deadline_expired=False) is V.WAIT
    assert _decide(recent, deadline_expired=True) is V.DEADLINE_QUEUED


def test_gated_behind_upstream_at_deadline_is_a_named_failure() -> None:
    assert _decide(None, deadline_expired=False) is V.WAIT
    assert _decide(None, deadline_expired=True) is V.DEADLINE_UPSTREAM
