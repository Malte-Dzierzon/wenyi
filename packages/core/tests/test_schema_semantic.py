"""Schema round-trips and semantic invariants for AI-processing reliability.

Pydantic contracts (ReviewRun/ReviewItem/ChapterSegments/ProjectDetail):
defaults, extra-field handling, and coercion. Semantic checks: translation
preserves segment count and order, glossary upserts preserve source identity,
resume never duplicates completed segments, events carry required keys.
"""

from __future__ import annotations

from wenyi_backend.schemas import ChapterSegments, EventOut, ProjectDetail, ReviewItem, ReviewRun
from wenyi_core.agents.translator import Translator
from wenyi_core.config import Config
from wenyi_core.events import TranslationEvent, make_progress_fn
from wenyi_core.glossary.store import GlossaryStore, GlossaryTerm
from wenyi_core.ingest.models import Segment
from wenyi_core.llm.providers.fake import FakeClient
from wenyi_core.pipeline.translation_batch import resume_batches


def _translator(handler, retry_limit: int = 0) -> tuple[Translator, FakeClient]:
    config = Config.from_dict(
        {
            "language": {"source": "ja", "target": "zh"},
            "llm": {
                "preset": "fake",
                "models": {
                    "default_strong": {"provider": "default", "model": "p"},
                    "default_cheap": {"provider": "default", "model": "f"},
                },
            },
            "pipeline": {"align_retry_limit": retry_limit},
        }
    )
    client = FakeClient(handler=handler)
    return Translator(client, config), client


# Schema round-trips: ReviewRun / ReviewItem.


def test_review_item_defaults():
    item = ReviewItem.model_validate(
        {"id": "r1", "kind": "issue", "status": "pending"}
    )
    assert item.type == ""
    assert item.detail == ""
    assert item.location is None
    assert item.evidence == []
    assert item.changes == []
    assert item.publications == []


def test_review_item_round_trip():
    payload = {
        "id": "r1",
        "kind": "issue",
        "type": "missing",
        "detail": "omitted clause",
        "suggestion": "restore it",
        "status": "pending",
        "location": {
            "chapter": 2,
            "text_index": 7,
            "segment_index": 3,
            "chapter_title": "ch",
            "source": "src",
            "current_target": "tgt",
        },
        "evidence": [],
        "issue": {"index": 3},
        "changes": [{"op": "edit"}],
        "publications": [{"stage": "review"}],
    }
    item = ReviewItem.model_validate(payload)
    assert ReviewItem.model_validate(item.model_dump()) == item
    assert item.location is not None and item.location.chapter == 2


def test_review_item_extra_fields_ignored():
    item = ReviewItem.model_validate(
        {"id": "r1", "kind": "change", "status": "fixed", "future_field": "x"}
    )
    assert not hasattr(item, "future_field")
    assert "future_field" not in item.model_dump()


def test_review_item_index_coercion():
    item = ReviewItem.model_validate({"id": "r1", "kind": "issue", "status": "pending"})
    assert isinstance(item.id, str)
    loc_payload = {
        "chapter": "2",
        "text_index": "7",
        "segment_index": "3",
        "chapter_title": "ch",
        "source": "src",
        "current_target": None,
    }
    loc_item = ReviewItem.model_validate(
        {"id": "r1", "kind": "issue", "status": "pending", "location": loc_payload}
    )
    assert loc_item.location is not None
    assert loc_item.location.chapter == 2
    assert loc_item.location.text_index == 7


def test_review_run_defaults_and_round_trip():
    run = ReviewRun.model_validate({"id": "run1", "review_id": "rev1", "status": "running"})
    assert run.items == []
    assert run.issues == []
    assert run.autofix == {}
    assert run.created_at is None
    payload = {
        "id": "run1",
        "review_id": "rev1",
        "status": "done",
        "created_at": "2026-01-01T00:00:00Z",
        "issues": [{"index": 0}],
        "summary": {"total": 1},
        "items": [{"id": "i1", "kind": "issue", "status": "pending"}],
    }
    full = ReviewRun.model_validate(payload)
    assert ReviewRun.model_validate(full.model_dump()) == full
    assert len(full.items) == 1 and full.items[0].id == "i1"


# Schema round-trips: ChapterSegments / ProjectDetail.


def test_chapter_segments_defaults_and_round_trip():
    chapter = ChapterSegments.model_validate(
        {
            "index": 0,
            "segments": [
                {"index": 0, "source": "src0", "target": "tgt0"},
                {"index": 1, "source": "src1"},
            ],
        }
    )
    assert chapter.title == ""
    assert chapter.title_translated is None
    assert chapter.review_issues == []
    assert chapter.segments[1].target is None
    assert chapter.segments[0].kind == "text"
    assert ChapterSegments.model_validate(chapter.model_dump()) == chapter


def test_chapter_segments_index_coercion():
    chapter = ChapterSegments.model_validate(
        {"index": "3", "segments": [{"index": "0", "source": "s"}]}
    )
    assert chapter.index == 3
    assert chapter.segments[0].index == 0


def test_project_detail_defaults_and_round_trip():
    project = ProjectDetail.model_validate({"id": "p1", "name": "book"})
    assert project.book_title is None
    assert project.chapter_count == 0
    assert project.total_word_count == 0
    assert project.done_chapters == 0
    assert project.source_meta == {}
    assert project.status == "created"
    full = ProjectDetail.model_validate(
        {
            "id": "p1",
            "name": "book",
            "book_title": "title",
            "chapter_count": 4,
            "total_word_count": 100,
            "done_chapters": 2,
            "source_meta": {"fmt": "txt"},
        }
    )
    assert ProjectDetail.model_validate(full.model_dump()) == full


def test_project_detail_extra_fields_ignored():
    project = ProjectDetail.model_validate({"id": "p1", "name": "b", "unknown": 1})
    assert "unknown" not in project.model_dump()


# Semantic: translation preserves segment count and order.


def test_translation_preserves_count_and_order():
    import json as _json
    import re as _re

    def handler(messages, tier, json_mode):
        n = len(_re.findall(r"^\[(\d+)\]", messages[-1]["content"], _re.MULTILINE))
        return _json.dumps({"translations": [f"訳{i}" for i in range(n)]})

    translator, _ = _translator(handler)
    sources = ["ああ", "いい", "うう", "ええ"]
    out = translator.translate_batch(sources)
    assert len(out) == len(sources)
    assert out == ["訳0", "訳1", "訳2", "訳3"]


def test_translation_keeps_nonlinguistic_positions_in_order():
    import json as _json

    seen = {}

    def handler(messages, tier, json_mode):
        seen["n_calls"] = seen.get("n_calls", 0) + 1
        return _json.dumps({"translations": ["訳本文"]})

    translator, _ = _translator(handler)
    out = translator.translate_batch(["-", "本文ほん", "42"])
    assert len(out) == 3
    assert out[0] == "-"
    assert out[1] == "訳本文"
    assert out[2] == "42"


# Semantic: glossary upserts preserve source identity.


def test_glossary_upsert_new_and_same_target_unchanged(tmp_path):
    store = GlossaryStore(str(tmp_path / "g.db"))
    try:
        assert store.upsert_term(GlossaryTerm(source="太郎", target="太郎君")) == "inserted"
        assert store.upsert_term(GlossaryTerm(source="太郎", target="太郎君")) == "unchanged"
        assert store.get_term("太郎") is not None
        assert store.get_term("太郎").target == "太郎君"
    finally:
        store.close()


def test_glossary_upsert_conflict_preserves_established_target(tmp_path):
    store = GlossaryStore(str(tmp_path / "g.db"))
    try:
        assert store.upsert_term(GlossaryTerm(source="安", target="安娜")) == "inserted"
        assert store.upsert_term(GlossaryTerm(source="安", target="安妮")) == "conflict"
        kept = store.get_term("安")
        assert kept is not None
        assert kept.source == "安"
        assert kept.target == "安娜"
        assert len(store.open_conflicts()) == 1
    finally:
        store.close()


def test_glossary_upsert_merges_aliases_without_identity_change(tmp_path):
    store = GlossaryStore(str(tmp_path / "g.db"))
    try:
        store.upsert_term(GlossaryTerm(source="王", target="老王", aliases=["a"]))
        assert (
            store.upsert_term(GlossaryTerm(source="王", target="老王", aliases=["b"]))
            == "unchanged"
        )
        term = store.get_term("王")
        assert term is not None
        assert term.source == "王" and term.target == "老王"
        assert set(term.aliases) == {"a", "b"}
    finally:
        store.close()


# Semantic: resume never duplicates completed segments.


def _segments(*states: str) -> list[Segment]:
    segments = []
    for index, state in enumerate(states):
        target = {"done": f"tgt{index}", "blank": ""}[state] if state != "pending" else None
        segments.append(Segment(index=index, source=f"src{index}", target=target))
    return segments


def test_resume_batches_never_mix_done_and_pending():
    segments = _segments("done", "done", "pending", "pending")
    batches = resume_batches(segments, 100000)
    assert [[s.index for s in batch] for batch in batches] == [[0, 1], [2, 3]]


def test_resume_batches_never_duplicate_completed_segments():
    segments = _segments("pending", "done", "pending", "done", "pending")
    batches = resume_batches(segments, 100000)
    flat = [s.index for batch in batches for s in batch]
    assert sorted(flat) == [0, 1, 2, 3, 4]
    assert len(flat) == len(set(flat))
    done_batches = [b for b in batches if b[0].target is not None]
    for batch in done_batches:
        assert all(s.target is not None for s in batch)


def test_resume_blank_string_counts_as_completed():
    segments = _segments("blank", "pending")
    batches = resume_batches(segments, 100000)
    assert [[s.index for s in batch] for batch in batches] == [[0], [1]]
    assert batches[0][0].target == ""


# Semantic: events carry required keys.


def test_translation_event_carries_required_keys():
    event = TranslationEvent(project_id="p1", kind="batch", done=2, total=5, label="t")
    for key in ("project_id", "kind", "done", "total", "label", "payload"):
        assert hasattr(event, key)
    assert event.done == 2 and event.total == 5 and event.label == "t"


def test_progress_fn_emits_structured_events():
    seen: list[TranslationEvent] = []

    class Emitter:
        def emit(self, event: TranslationEvent) -> None:
            seen.append(event)

    fn = make_progress_fn(Emitter(), project_id="p1", kind="progress")
    fn(1, 4, "translating")
    fn(4, 4, "done")
    assert len(seen) == 2
    for event in seen:
        assert event.project_id == "p1"
        assert isinstance(event.kind, str) and event.kind
        assert isinstance(event.done, int) and isinstance(event.total, int)
        assert isinstance(event.label, str)
    assert (seen[0].done, seen[0].total) == (1, 4)
    assert (seen[1].done, seen[1].total) == (4, 4)


def test_event_out_schema_round_trip_with_payload_default():
    event = EventOut.model_validate({"id": 7, "type": "batch"})
    assert event.payload == {}
    assert event.created_at is None
    full = EventOut.model_validate(
        {"id": 7, "type": "batch", "payload": {"done": 1, "total": 2}}
    )
    assert EventOut.model_validate(full.model_dump()) == full
    assert full.payload["total"] == 2
