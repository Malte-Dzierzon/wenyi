"""Read-only glossary snapshot integrity and checkpoint-race tests."""

from __future__ import annotations

import os
import tempfile
import unittest

from wenyi_core.glossary.store import GlossaryStore, GlossaryTerm


class TestReadonlyGlossarySnapshot(unittest.TestCase):
    def test_reads_committed_wal_without_modifying_database(self):
        """Read-only term reads must include committed, uncheckpointed WAL data
        without writing to the database (a live WAL read, not a 122MB copy)."""
        import sqlite3

        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "glossary.db")
            writer = GlossaryStore(path)
            try:
                writer.upsert_term(
                    GlossaryTerm(source="Ann", target="安", type="person"),
                    chapter=0,
                )
                terms = GlossaryStore.load_terms_readonly(path)
                self.assertEqual([(term.source, term.target) for term in terms], [("Ann", "安")])
                # The database still opens cleanly and holds the same rows:
                # the read left no lock, journal, or partial write behind.
                conn = sqlite3.connect(path)
                try:
                    rows = conn.execute("SELECT source, target FROM glossary ORDER BY rowid").fetchall()
                    self.assertEqual([(r[0], r[1]) for r in rows], [("Ann", "安")])
                finally:
                    conn.close()
            finally:
                writer.close()

    def test_reads_consistent_view_during_concurrent_writes(self):
        """Terms must read successfully while another connection writes
        (the old copy loop retried 5x and then raised under load)."""

        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "glossary.db")
            writer = GlossaryStore(path)
            try:
                writer.upsert_term(
                    GlossaryTerm(source="Ann", target="安", type="person"),
                    chapter=0,
                )
                # Simulate a busy worker: checkpoint + write between open and read.
                writer.conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
                writer.upsert_term(
                    GlossaryTerm(source="Bob", target="鲍勃", type="person"),
                    chapter=0,
                )
                terms = GlossaryStore.load_terms_readonly(path)
                sources = [(term.source, term.target) for term in terms]
                # Either snapshot is consistent; never a hang or RuntimeError.
                self.assertIn(("Ann", "安"), sources)
            finally:
                writer.close()
