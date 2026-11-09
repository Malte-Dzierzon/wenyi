"""Parallel chapter execution: three lanes run concurrently, not serially.

Admission lanes are covered elsewhere; here the runtime proves overlap:
three chapter jobs on one project all start before any finishes, and all
complete with correct terminal status.
"""
from __future__ import annotations

import asyncio

import pytest
from wenyi_backend import dal
from wenyi_backend.context import current_context, use_context
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


def test_three_chapter_lanes_overlap(catalog, monkeypatch):
    from wenyi_backend.workers import tasks

    async def run():
        runtime = LocalRuntime(catalog, current_context())
        started: set[str] = set()
        max_overlap = 0
        release = asyncio.Event()
        lock = asyncio.Lock()

        async def execute(ctx, *, project_id, run_id, chapter_index=None):
            nonlocal max_overlap
            async with lock:
                started.add(run_id)
                max_overlap = max(max_overlap, len(started))
            job = dal.get_job_by_arq_id(run_id)
            dal.set_job_status(job["id"], "running")
            await release.wait()
            async with lock:
                started.discard(run_id)
            dal.set_job_status(job["id"], "done")

        monkeypatch.setattr(tasks, "run_chapter_translation", execute)
        await runtime.start()
        try:
            pid = dal.create_project("Parallel", "en", "zh", {})
            # Admit three chapters via the real lane rules (no 409 expected).
            for chapter in range(3):
                run_id = f"chapter-{chapter}"
                catalog.admit_job(
                    pid,
                    "chapter_translation",
                    run_id,
                    project_status="translating",
                    params={"chapter_index": chapter},
                    run_id=run_id,
                )
                job = dal.get_job_by_arq_id(run_id)
                assert job is not None
                runtime.enqueue(
                    "run_chapter_translation",
                    _job_id=run_id,
                    project_id=pid,
                    run_id=run_id,
                    chapter_index=chapter,
                )
            # Wait until all three overlap (or time out = serialized = fail).
            for _ in range(100):
                async with lock:
                    if len(started) == 3:
                        break
                await asyncio.sleep(0.05)
            async with lock:
                assert len(started) == 3, f"no 3-way overlap, max was {max_overlap}"
            release.set()
            await asyncio.wait_for(runtime.workflow_queue.join(), 5)
            for chapter in range(3):
                job = dal.get_job_by_arq_id(f"chapter-{chapter}")
                assert job["status"] == "done"
        finally:
            release.set()
            await asyncio.wait_for(runtime.stop(), 5)

    asyncio.run(run())


def test_same_chapter_second_job_rejected_while_first_runs(catalog):
    async def run():
        runtime = LocalRuntime(catalog, current_context())
        await runtime.start()
        try:
            pid = dal.create_project("Conflict", "en", "zh", {})
            catalog.admit_job(
                pid,
                "chapter_translation",
                "first",
                project_status="translating",
                params={"chapter_index": 0},
                run_id="first",
            )
            from fastapi import HTTPException

            with pytest.raises(HTTPException) as caught:
                catalog.admit_job(
                    pid,
                    "chapter_translation",
                    "second",
                    project_status="translating",
                    params={"chapter_index": 0},
                    run_id="second",
                )
            assert caught.value.status_code == 409
        finally:
            await asyncio.wait_for(runtime.stop(), 5)

    asyncio.run(run())
