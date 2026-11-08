"""Background glossary curator: occurrence search and curation passes."""

from __future__ import annotations

import json
import tempfile
import unittest

from wenyi_core.config import Config
from wenyi_core.glossary.extractor import TranslatedSegmentEvidence
from wenyi_core.glossary.store import GlossaryTerm
from wenyi_core.ingest.models import Chapter, Segment
from wenyi_core.llm.providers.fake import FakeClient
from wenyi_core.pipeline.curator import (
    CURATOR_BATCH_SIZE,
    CuratorService,
    find_occurrences,
)
from wenyi_core.storage.sqlite import SqliteStorage


def _cfg():
    return Config.from_dict(
        {
            "language": {"source": "en", "target": "de"},
            "llm": {
                "preset": "fake",
                "models": {
                    "default_strong": {"provider": "default", "model": "p"},
                    "default_cheap": {"provider": "default", "model": "f"},
                },
            },
        }
    )


def _chapter(index: int, sources: list[str]) -> Chapter:
    return Chapter(
        index=index,
        title=f"ch{index}",
        segments=[Segment(index=i, source=s) for i, s in enumerate(sources)],
    )


class TestFindOccurrences(unittest.TestCase):
    def test_positions_aliases_and_targets(self):
        term = GlossaryTerm(source="Klein", target="Klein", aliases=["Herr Klein"])
        chapters = [
            _chapter(0, ["Herr Klein ging.", "Niemand da.", "Klein blieb."]),
            _chapter(1, ["Ohne Treffer."]),
        ]
        history = {
            (0, 0): TranslatedSegmentEvidence(
                chapter=0, segment=0, source="Herr Klein ging.", target="Mr Klein left."
            ),
        }
        found = find_occurrences(term, chapters, history)
        self.assertEqual([(o.chapter, o.segment) for o in found], [(0, 0), (0, 2)])
        self.assertEqual(found[0].target, "Mr Klein left.")
        self.assertIsNone(found[1].target)
        # "Klein" is checked before its alias and already matches the sentence.
        self.assertEqual(found[0].matched, "Klein")

    def test_limit_picks_first_last_and_middle(self):
        term = GlossaryTerm(source="x", target="x")
        chapters = [_chapter(0, [f"seg x {i}" for i in range(20)])]
        found = find_occurrences(term, chapters, limit=5)
        self.assertEqual(len(found), 5)
        self.assertEqual(found[0].segment, 0)
        self.assertEqual(found[-1].segment, 19)


class TestCuratorPass(unittest.TestCase):
    def _store(self, directory: str) -> SqliteStorage:
        store = SqliteStorage(directory, create=True)
        store.write_artifact(
            "manifest.json",
            {"title": "book", "chapters": [{"index": 0, "status": "pending"}]},
        )
        from wenyi_core.ingest.models import Chapter as Ch

        store.write_artifact("chapters/ch0.json", Ch(index=0, title="ch0").model_dump())
        return store

    def test_conflict_verdict_files_conflict(self):
        verdict = {"status": "conflict", "proposed_target": "KleinB", "reason": "differs"}
        client = FakeClient(handler=lambda m, t, j: json.dumps(verdict))
        curator = CuratorService(client, _cfg())
        with tempfile.TemporaryDirectory() as directory:
            store = self._store(directory)
            try:
                term = GlossaryTerm(source="Klein", target="Klein")
                store.upsert_term(term, 0)
                stats = curator.run_pass(store, [term], [], {})
                self.assertEqual(stats, {"checked": 1, "verified": 0, "conflicts": 1})
                conflicts = store.open_conflicts()
                self.assertEqual(len(conflicts), 1)
                self.assertIn("curator:", conflicts[0].get("note", ""))
            finally:
                store.close()

    def test_verified_verdict_files_nothing(self):
        client = FakeClient(handler=lambda m, t, j: json.dumps({"status": "verified"}))
        curator = CuratorService(client, _cfg())
        with tempfile.TemporaryDirectory() as directory:
            store = self._store(directory)
            try:
                term = GlossaryTerm(source="Klein", target="Klein")
                store.upsert_term(term, 0)
                stats = curator.run_pass(store, [term], [], {})
                self.assertEqual(stats, {"checked": 1, "verified": 1, "conflicts": 0})
                self.assertEqual(store.open_conflicts(), [])
            finally:
                store.close()

    def test_stop_flag_aborts_pass(self):
        client = FakeClient(handler=lambda m, t, j: json.dumps({"status": "verified"}))
        curator = CuratorService(client, _cfg())
        with tempfile.TemporaryDirectory() as directory:
            store = self._store(directory)
            try:
                terms = [GlossaryTerm(source=f"t{i}", target=f"t{i}") for i in range(3)]
                store.write_artifact("curator_state.json", {"stop_requested": True})
                stats = curator.run_pass(store, terms, [], {})
                self.assertEqual(stats["checked"], 0)
                self.assertEqual(len(client.calls), 0)
            finally:
                store.close()

    def test_batch_size_bounds_pass(self):
        self.assertEqual(CURATOR_BATCH_SIZE, 20)
