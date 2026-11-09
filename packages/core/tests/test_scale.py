"""Scale tests: large books, glossaries, and event logs stay correct and fast.

Reproducible synthetic data (seeded Random). Bounds are generous — these
catch O(n^2) blowups and correctness breaks at scale, not micro-benchmarks.
"""
from __future__ import annotations

import random
import tempfile
import time

from wenyi_core.glossary.store import GlossaryStore, GlossaryTerm
from wenyi_core.ingest.models import Chapter, Segment
from wenyi_core.storage.file import FileStorage
from wenyi_core.storage.sqlite import SqliteStorage


def _terms(n: int, seed: int = 7) -> list[GlossaryTerm]:
    rng = random.Random(seed)
    surnames = ["Mustername", "Beispiel", "Probst", "Klein", "Groß"]
    return [
        GlossaryTerm(
            source=f"Begriff{i} {rng.choice(surnames)}",
            target=f"Term{i}",
            type="name",
            aliases=[f"Alias{i}"] if i % 3 == 0 else [],
        )
        for i in range(n)
    ]


def _plain_terms(n: int) -> list[GlossaryTerm]:
    """Deterministic names so batch text hits exactly the expected terms."""
    return [
        GlossaryTerm(source=f"Begriff{i} Mustername", target=f"Term{i}", type="name")
        for i in range(n)
    ]
def test_terms_in_10k_scales_linearly():
    terms = _plain_terms(10000)
    batch = " ".join(
        f"Der Begriff{i} Mustername ging nach Hause." for i in range(0, 10000, 100)
    )
    started = time.monotonic()
    hits = GlossaryStore.terms_in(terms, batch)
    elapsed = time.monotonic() - started
    assert len(hits) == 100
    # Prefilter keeps this in ms; a regex-per-term scan would take seconds.
    assert elapsed < 2.0, f"terms_in(10k) took {elapsed:.2f}s"


def test_terms_in_result_matches_naive_scan():
    """Filtered result equals the naive per-term check on the same data."""
    from wenyi_core.glossary.store import source_matches_text, term_match_sources

    terms = _terms(500)
    batch = "Begriff7 Klein traf Alias12 und Begriff499 Groß im Garten."
    fast = GlossaryStore.terms_in(terms, batch)
    naive = [
        t
        for t in terms
        if any(source_matches_text(k, batch) for k in term_match_sources(t))
    ]
    assert [t.source for t in fast] == [t.source for t in naive]


def _big_chapter(index: int, segments: int, seed: int) -> Chapter:
    rng = random.Random(seed)
    words = ["Der", "schnelle", "Fuchs", "springt", "über", "den", "faulen", "Hund"]
    return Chapter(
        index=index,
        title=f"Chapter {index}",
        segments=[
            Segment(
                index=j,
                source=" ".join(rng.choice(words) for _ in range(30)),
                target=" ".join(rng.choice(words) for _ in range(30)),
            )
            for j in range(segments)
        ],
    )


def test_sqlite_chapter_roundtrip_5k_segments():
    with tempfile.TemporaryDirectory() as d:
        store = SqliteStorage(d, create=True)
        try:
            store.save_manifest({"chapters": [{"index": 0, "title": "Big"}]})
            chapter = _big_chapter(0, 5000, seed=11)
            started = time.monotonic()
            store.save_chapter(chapter)
            save_s = time.monotonic() - started
            started = time.monotonic()
            loaded = store.load_chapter(0)
            load_s = time.monotonic() - started
            assert len(loaded.text_segments) == 5000
            assert loaded.text_segments[4999].target is not None
            assert save_s < 10.0 and load_s < 10.0, (save_s, load_s)
        finally:
            store.close()


def test_file_events_tail_matches_full_at_scale():
    with tempfile.TemporaryDirectory() as d:
        store = FileStorage(f"{d}/run")
        for i in range(3000):
            store.log_event("batch_translated", chapter=1, start_index=i, payload="x" * 500)
        started = time.monotonic()
        tail = store.list_events(limit=200)
        tail_s = time.monotonic() - started
        full = store.list_events(limit=0)
        assert len(full) == 3000
        assert tail == full[-200:]
        assert tail_s < 2.0, f"tail read took {tail_s:.2f}s"


def test_glossary_sqlite_10k_terms_roundtrip():
    with tempfile.TemporaryDirectory() as d:
        store = SqliteStorage(d, create=True)
        try:
            terms = _terms(10000)
            started = time.monotonic()
            for term in terms:
                store.upsert_term(term, chapter=0)
            write_s = time.monotonic() - started
            started = time.monotonic()
            loaded = store.all_terms()
            read_s = time.monotonic() - started
            assert len(loaded) == 10000
            assert loaded[9999].source.startswith("Begriff9999")
            assert write_s < 60.0 and read_s < 10.0, (write_s, read_s)
        finally:
            store.close()
