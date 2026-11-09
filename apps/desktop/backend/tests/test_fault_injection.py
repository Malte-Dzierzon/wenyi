"""Fault injection: worker crash, provider outage, and queue rejection.

Proves durable state survives: interrupted jobs are marked once,
provider errors surface as job errors (not hangs), and failed queue
submissions restore the previous project status.
"""
from __future__ import annotations

import asyncio
from datetime import datetime, timedelta, timezone

import pytest
from wenyi_backend import dal
from wenyi_backend.context import current_context, use_context
from wenyi_core.llm.retrying import EmptyResponseError
from wenyi_desktop.local_runtime import LocalRuntime
from wenyi_desktop.main import create_context


@pytest.fixture
def catalog(tmp_path):
    context = create_context(tmp_path / "desktop")
    backend = context.repository
    try:
        with use_context(context):
            yield backend
    finally:
        backend.close()


def test_crash_recovery_interrupts_stale_jobs_once(catalog):
    async def run():
        runtime = LocalRuntime(catalog, current_context())
        await runtime.start()
        try:
            pid = dal.create_project("Crash", "en", "zh", {})
            job = dal.create_job(pid, "translation", "crashed", run_id="crashed")
            dal.set_job_status(job, "running")
            # Backdate past the 120s grace so periodic recovery picks it up.
            old = (datetime.now(timezone.utc) - timedelta(seconds=300)).isoformat()
            catalog._put(
                "jobs",
                job,
                {**dal.get_job(job), "updated_at": old},
            )
            await asyncio.to_thread(runtime.recover)
            assert dal.get_job(job)["status"] == "interrupted"
            # Second recovery is a no-op (idempotent).
            await asyncio.to_thread(runtime.recover)
            assert dal.get_job(job)["status"] == "interrupted"
        finally:
            await asyncio.wait_for(runtime.stop(), 5)

    asyncio.run(run())


def test_provider_error_fails_job_without_hang(catalog, monkeypatch):
    from wenyi_backend.workers import tasks

    async def run():
        runtime = LocalRuntime(catalog, current_context())

        def boom(*args, **kwargs):
            raise EmptyResponseError("provider down")

        monkeypatch.setattr(tasks, "_execute", boom)
        await runtime.start()
        try:
            pid = dal.create_project("Outage", "en", "zh", {})
            identity = dal.create_job(pid, "translation", "outage", run_id="outage")
            runtime.enqueue(
                "run_translation", _job_id="outage", project_id=pid, run_id="outage"
            )
            await asyncio.wait_for(runtime.workflow_queue.join(), 10)
            job = dal.get_job(identity)
            assert job["status"] in {"error", "interrupted"}, job["status"]
        finally:
            await asyncio.wait_for(runtime.stop(), 5)

    asyncio.run(run())


def test_queue_rejection_marks_job_and_restores_status(catalog):
    async def run():
        runtime = LocalRuntime(catalog, current_context())
        try:
            pid = dal.create_project("Reject", "en", "zh", {})
            assert dal.get_project(pid)["status"] == "created"
            run_id = "rejected"
            identity = catalog.admit_job(
                pid,
                "translation",
                run_id,
                project_status="translating",
                run_id=run_id,
            )
            assert dal.get_project(pid)["status"] == "translating"
            # Simulate queue rejection after admission.
            ok = catalog.fail_admission(identity, "created", "queue full")
            assert ok is True
            assert dal.get_job(identity)["status"] == "error"
            assert dal.get_project(pid)["status"] == "created"
        finally:
            await asyncio.wait_for(runtime.stop(), 5)

    asyncio.run(run())


@pytest.mark.parametrize("failing", [True, False])
def test_curator_never_touches_project_status(catalog, failing):
    """Curator success/failure records only its job row, never the project.
    A looping curator model must not paint the project red every 5 minutes.
    """
    from wenyi_backend.workers import tasks

    pid = dal.create_project("Isolation", "en", "zh", {})
    dal.set_project_status(pid, "paused")
    run_id = "curator-iso"
    catalog.admit_job(
        pid,
        "glossary_curator",
        run_id,
        project_status="curating",
        run_id=run_id,
    )
    job = dal.get_job_by_arq_id(run_id)
    # Emulate the worker outcome path without running a full pass.
    if failing:
        tasks._set_curator_job_status(run_id, "error", RuntimeError("truncated"))
        assert dal.get_job(job["id"])["status"] == "error"
    else:
        tasks._set_curator_job_status(run_id, "done")
        assert dal.get_job(job["id"])["status"] == "done"
    # Project status untouched by curator outcome (admission set
    # it; the worker must not overwrite it with error/curated).
    project = dal.get_project(pid)
    assert project["status"] == "curating", project
