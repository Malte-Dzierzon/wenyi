"""Scale and parallelism regression tests (offline, deterministic).

Covers glossary matching at 5000 terms, 10k-event log tails, parallel
chapter translation on SQLite, 200-batch interrupt/resume and the
oversize-prompt gate. No live network; sleeps are real sleeps nowhere.
"""

from __future__ import annotations

import random
import threading
import time

import pytest
from wenyi_core.config import Config
from wenyi_core.glossary.store import GlossaryStore, GlossaryTerm
from wenyi_core.ingest.models import Chapter, Document, Segment
from wenyi_core.llm.providers.fake import FakeProvider
from wenyi_core.llm.router import RoutedLLMClient
from wenyi_core.pipeline.context import RollingContext
from wenyi_core.pipeline.orchestrator import Orchestrator
from wenyi_core.pipeline.runstore import STATUS_DONE
from wenyi_core.storage.sqlite import SqliteStorage

from tests.fake_llm import MeteredFakeClient, routing_handler
from tests.pipeline_fixtures import fake_pipeline_config as _base_config


def _translation_config(state_dir: str, *, batch_tokens: int = 1800) -> Config:
    return Config.from_dict(
        {
            "language": {"source": "ja", "target": "zh"},
            "llm": {"preset": "fake"},
            "segment": {"max_tokens_per_batch": batch_tokens},
            "pipeline": {
                "review": False,
                "polish": False,
                "book_understanding": False,
                "review_autofix": False,
                "annotation_alignment": False,
            },
            "paths": {"state_dir": state_dir},
        }
    )


def _write_source(tmp_path, name: str, text: str) -> str:
    path = str(tmp_path / name)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(text)
    return path


def test_terms_in_5000_scale_time_bound():
    rng = random.Random(0)
    terms = [
        GlossaryTerm(source=f"term_{i:04d}", target=f"译{i:04d}") for i in range(5000)
    ]
    # A few terms carry aliases; matching must still resolve through the source list.
    terms[7] = GlossaryTerm(source="term_0007", target="译0007", aliases=["t7"])
    text = "story term_0007 middle term_1999 end term_4999."
    assert rng.random() < 1.0  # Seeded fixture marker; corpus above is fixed.
    started = time.monotonic()
    found = GlossaryStore.terms_in(terms, text)
    elapsed = time.monotonic() - started
    assert {term.source for term in found} == {"term_0007", "term_1999", "term_4999"}
    assert elapsed < 3.0


def test_terms_in_alias_and_boundary_matching():
    terms = [
        GlossaryTerm(source="綾小路", target="绫小路"),
        GlossaryTerm(source="Knight", target="骑士", aliases=["Ann"]),
    ]
    assert [t.source for t in GlossaryStore.terms_in(terms, "綾小路登场")] == ["綾小路"]
    assert [t.source for t in GlossaryStore.terms_in(terms, "Ann went home")] == ["Knight"]
    # ASCII matching respects word boundaries: Anna must not match Ann.
    assert GlossaryStore.terms_in(terms, "Anna went home") == []
    assert GlossaryStore.terms_in(terms, "") == []


def test_event_log_10k_tail_correctness_and_speed(tmp_path):
    store = SqliteStorage(str(tmp_path / "run"), create=True)
    try:
        total = 10_000
        for index in range(total):
            store.log_event("bench_event", seq=index)
        started = time.monotonic()
        tail = store.list_events(limit=100)
        elapsed = time.monotonic() - started
        assert len(tail) == 100
        assert [event["seq"] for event in tail] == list(range(total - 100, total))
        ids = [event["_id"] for event in tail]
        assert ids == sorted(ids)
        assert elapsed < 3.0
    finally:
        store.close()


def test_event_log_type_filter_tail(tmp_path):
    store = SqliteStorage(str(tmp_path / "run"), create=True)
    try:
        for index in range(500):
            store.log_event("type_a" if index % 2 else "type_b", seq=index)
        started = time.monotonic()
        tail = store.list_events(event_type="type_a", limit=50)
        elapsed = time.monotonic() - started
        assert [event["seq"] for event in tail] == list(range(401, 500, 2))
        assert all(event["event"] == "type_a" for event in tail)
        assert elapsed < 2.0
    finally:
        store.close()


def test_event_log_empty_store(tmp_path):
    store = SqliteStorage(str(tmp_path / "run"), create=True)
    try:
        assert store.list_events() == []
        assert store.list_events(event_type="missing", limit=10) == []
    finally:
        store.close()


def _chapter(index: int, sentences: int) -> Chapter:
    return Chapter(
        index=index,
        title=f"Chapter {index}",
        segments=[
            Segment(
                index=position,
                source=f"Section {index} sentence {position} describing the quiet village.",
            )
            for position in range(sentences)
        ],
    )


def _init_book(tmp_path, name: str, chapters: list[Chapter]) -> tuple[str, str]:
    run_dir = str(tmp_path / name)
    source_path = _write_source(tmp_path, f"{name}.txt", "source\n")
    store = SqliteStorage(run_dir, create=True)
    try:
        store.init_from_document(
            Document(
                title=name,
                source_lang="ja",
                target_lang="zh",
                fmt="text",
                source_path=source_path,
                chapters=chapters,
            )
        )
    finally:
        store.close()
    return run_dir, source_path


def test_parallel_chapter_translate_usage_merge_exact(tmp_path):
    """Three threads translate distinct chapters; no usage increment is lost."""
    config = _translation_config(str(tmp_path / "state"))
    run_dir, _ = _init_book(tmp_path, "parallel", [_chapter(0, 6), _chapter(1, 6), _chapter(2, 6)])
    barrier = threading.Barrier(3)
    summaries: list[dict] = []
    errors: list[BaseException] = []
    guard = threading.Lock()

    def worker(chapter_index: int) -> None:
        store = SqliteStorage(run_dir, create=False)
        try:
            client = MeteredFakeClient(handler=routing_handler)
            orch = Orchestrator(config, client=client)
            try:
                orch._translation.translate_chapter(
                    chapter_index,
                    store,
                    store,
                    RollingContext(),
                    "",
                    translation_history={},
                    source_corpus="",
                    annotation_context_registry=None,
                    done=0,
                    total=18,
                )
            except BaseException as error:  # noqa: BLE001 - collected for the main thread.
                with guard:
                    errors.append(error)
                barrier.abort()
                return
            try:
                barrier.wait(timeout=60)
            except threading.BrokenBarrierError:
                return
            try:
                orch._runtime.flush_usage(store, scope="translate")
            except BaseException as error:  # noqa: BLE001 - collected for the main thread.
                with guard:
                    errors.append(error)
                return
            with guard:
                summaries.append(client.usage_summary())
        finally:
            store.close()

    threads = [threading.Thread(target=worker, args=(ci,)) for ci in range(3)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=120)
    assert not any(thread.is_alive() for thread in threads)
    assert errors == []

    reader = SqliteStorage(run_dir, create=False)
    try:
        cumulative = reader.load_usage()
        manifest = reader.load_manifest()
        chapters = {entry["index"]: reader.load_chapter(entry["index"]) for entry in manifest["chapters"]}
    finally:
        reader.close()

    assert cumulative is not None
    expected_calls = sum(summary["totals"]["calls"] for summary in summaries)
    expected_tokens = sum(summary["totals"]["total_tokens"] for summary in summaries)
    assert expected_calls > 0
    assert cumulative["totals"]["calls"] == expected_calls
    assert cumulative["totals"]["total_tokens"] == expected_tokens
    for chapter in chapters.values():
        assert all(segment.target for segment in chapter.text_segments)
    assert all(entry["status"] == STATUS_DONE for entry in manifest["chapters"])


def test_long_run_200_batch_interrupt_resume_completes(tmp_path):
    """Interrupt after 60 batches; resume finishes all 200 without retranslation."""
    config = _translation_config(str(tmp_path / "state"), batch_tokens=2)
    sources = [f"Source sentence number {i} for scale testing." for i in range(200)]
    run_dir, _ = _init_book(
        tmp_path, "longrun", [_chapter(0, 0)]
    )
    # Replace the placeholder chapter with 200 real segments.
    setup = SqliteStorage(run_dir, create=False)
    try:
        chapter = setup.load_chapter(0)
        chapter.segments = [Segment(index=i, source=text) for i, text in enumerate(sources)]
        setup.save_chapter(chapter)
    finally:
        setup.close()

    state = {"translated": 0, "armed": True}

    def flaky(messages, tier, json_mode):
        if "literary translator" in messages[0]["content"]:
            state["translated"] += 1
            if state["armed"] and state["translated"] > 60:
                raise RuntimeError("simulated interrupt after 60 batches")
        return routing_handler(messages, tier, json_mode)

    store = SqliteStorage(run_dir, create=False)
    try:
        first = Orchestrator(config, client=MeteredFakeClient(handler=flaky))
        with pytest.raises(RuntimeError, match="simulated interrupt"):
            first._translation.translate_chapter(
                0,
                store,
                store,
                RollingContext(),
                "",
                translation_history={},
                source_corpus="",
                annotation_context_registry=None,
            )
        partial = store.load_chapter(0)
        assert sum(1 for s in partial.text_segments if s.target is not None) == 60

        second = Orchestrator(config, client=MeteredFakeClient(handler=routing_handler))
        second._translation.translate_chapter(
            0,
            store,
            store,
            RollingContext(),
            "",
            translation_history={},
            source_corpus="",
            annotation_context_registry=None,
        )
        body_calls = [c for c in first.client.calls if c["operation"] == "translation.body"]
        body_calls += [c for c in second.client.calls if c["operation"] == "translation.body"]
        # 60 persisted + 1 interrupted (never persisted, correctly repeated)
        # + 140 remaining = 201 calls for 200 batches.
        assert len(body_calls) == 201

        finished = store.load_chapter(0)
        assert len(finished.text_segments) == 200
        assert all(segment.target for segment in finished.text_segments)

        translated = store.list_events(event_type="batch_translated", chapter=0, limit=0)
        skipped = store.list_events(event_type="batch_skipped", chapter=0, limit=0)
        assert sorted(e["start_index"] for e in translated) == list(range(200))
        assert len(skipped) == 60
        assert store.load_manifest()["chapters"][0]["status"] == STATUS_DONE
    finally:
        store.close()


def test_resume_batches_partition_counts():
    segments = [Segment(index=i, source=f"s{i}") for i in range(10)]
    for i in range(4):
        segments[i].target = f"t{i}"
    batches = __import__(
        "wenyi_core.pipeline.translation_batch", fromlist=["resume_batches"]
    ).resume_batches(segments, 10_000)
    assert [len(batch) for batch in batches] == [4, 6]
    assert all(s.target is not None for s in batches[0])
    assert all(s.target is None for s in batches[1])


def test_prompt_gate_rejects_120k_scale_with_actionable_error(monkeypatch):
    from wenyi_core.llm.configuration import LLMConfig

    raw = {
        "providers": {"a": {"kind": "fake", "max_retries": 0}},
        "models": {
            "m": {
                "provider": "a",
                "model": "x",
                "max_output_tokens": 128,
                "max_input_tokens": 32000,
            }
        },
        "tiers": {"strong": "m", "cheap": "m", "fast": "m"},
        "routes": {"translation.body": {"model": "m"}},
    }
    client = RoutedLLMClient(LLMConfig.model_validate(raw))

    def must_not_send(self, *args, **kwargs):
        raise AssertionError("oversize prompt must not reach the transport")

    monkeypatch.setattr(FakeProvider, "_request", must_not_send)
    oversize = "word " * 120_000
    started = time.monotonic()
    with pytest.raises(ValueError) as excinfo:
        client.complete(
            [{"role": "user", "content": oversize}], operation="translation.body"
        )
    elapsed = time.monotonic() - started
    message = str(excinfo.value)
    assert "max_input_tokens=32000" in message
    assert "shrink the batch" in message
    assert "raise the model limit" in message
    assert elapsed < 5.0


def test_prompt_gate_under_limit_passes():
    from wenyi_core.llm.configuration import LLMConfig

    raw = {
        "providers": {"a": {"kind": "fake", "max_retries": 0}},
        "models": {
            "m": {
                "provider": "a",
                "model": "x",
                "max_output_tokens": 128,
                "max_input_tokens": 32000,
            }
        },
        "tiers": {"strong": "m", "cheap": "m", "fast": "m"},
        "routes": {"translation.body": {"model": "m"}},
    }
    client = RoutedLLMClient(LLMConfig.model_validate(raw))
    assert isinstance(
        client.complete([{"role": "user", "content": "hi"}], operation="translation.body"),
        str,
    )


def test_base_config_import_used(tmp_path):
    assert _base_config(str(tmp_path / "state")).segment.max_tokens_per_batch == 1800
