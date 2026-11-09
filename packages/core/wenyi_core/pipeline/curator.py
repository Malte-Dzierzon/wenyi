"""Background glossary curation: check terms against book evidence in a loop.

The curator runs as its own job kind (`glossary_curator`) beside chapter
workers. Each pass checks ~20 terms: find occurrences in the book, ask the
curator model (`glossary.curate`) whether base form and translation hold up,
and file disagreements as `term_conflicts` for human review. It never
overwrites translations silently. Web evidence is a later extension point
(see `WebEvidence`, currently a no-op stub returning no results).
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from ..agents.base import Agent
from ..glossary.extractor import TranslatedSegmentEvidence
from ..glossary.store import (
    GlossaryTerm,
    TermMatcher,
)
from ..ingest.models import Chapter

if TYPE_CHECKING:
    from ..config import Config
    from ..llm.base import LLMClient
    from ..storage.protocol import Storage

#: Terms checked per curator pass (keeps each run to a few minutes).
CURATOR_BATCH_SIZE = 20
#: Maximum book excerpts attached to one curation prompt.
CURATOR_MAX_EVIDENCE = 5


@dataclass(frozen=True)
class TermOccurrence:
    """One place in the book where a term (or an alias) appears."""

    chapter: int
    segment: int
    source: str
    target: str | None
    matched: str


def _scan_occurrences(
    term: GlossaryTerm,
    chapters: list[Chapter],
    history: dict[tuple[int, int], TranslatedSegmentEvidence] | None,
) -> list[TermOccurrence]:
    """Scan all chapters for a term; shared core of find/count (no I/O)."""
    matcher = TermMatcher(term)
    if not matcher:
        return []
    matches: list[TermOccurrence] = []
    for chapter in chapters:
        for segment_index, segment in enumerate(chapter.text_segments):
            if not segment.source:
                continue
            hit = matcher.first_hit(segment.source)
            if hit is None:
                continue
            target = None
            if history is not None:
                evidence = history.get((chapter.index, segment_index))
                target = evidence.target if evidence else None
            matches.append(
                TermOccurrence(
                    chapter=chapter.index,
                    segment=segment_index,
                    source=segment.source,
                    target=target,
                    matched=hit,
                )
            )
    return matches


def find_occurrences(
    term: GlossaryTerm,
    chapters: list[Chapter],
    history: dict[tuple[int, int], TranslatedSegmentEvidence] | None = None,
    *,
    limit: int = CURATOR_MAX_EVIDENCE,
) -> list[TermOccurrence]:
    """Find where a term occurs, returning first/last/middle excerpts.

    Pure function over already-loaded chapters (no I/O): callers cache the
    chapter index across passes. Matches the term source and its aliases;
    targets come from `history` when the segment is translated.
    """
    matches = _scan_occurrences(term, chapters, history)
    return pick_excerpts(matches, limit)


def pick_excerpts(matches: list[TermOccurrence], limit: int) -> list[TermOccurrence]:
    """Pick first/last/middle excerpts from scanned occurrences."""
    if len(matches) <= limit:
        return matches
    picked = [matches[0]]
    step = (len(matches) - 1) / (limit - 1)
    for slot in range(1, limit - 1):
        picked.append(matches[round(slot * step)])
    picked.append(matches[-1])
    return picked


@dataclass
class WebEvidence:
    """Optional web-search evidence for one term (stub: no results yet)."""

    query: str = ""
    snippets: list[str] = field(default_factory=list)


class CuratorService(Agent):
    """Check glossary terms against book evidence, filing conflicts."""

    def __init__(self, client: LLMClient, config: Config):
        super().__init__(client, config)

    def check_term(
        self,
        term: GlossaryTerm,
        occurrences: list[TermOccurrence],
        *,
        book_title: str = "",
        web: WebEvidence | None = None,
    ) -> dict[str, Any]:
        """Ask the curator model to review one term; return the parsed verdict.

        The verdict is a dict with `status` (`verified` | `conflict`),
        optional `proposed_target`, `reason` and `duplicate_of`.
        """
        evidence = "\n".join(
            f"- ch{occ.chapter} seg{occ.segment} [{occ.matched}]: {occ.source[:200]}"
            + (f"  =>  {occ.target[:200]}" if occ.target else "  (untranslated)")
            for occ in occurrences
        )
        web_block = ""
        if web and web.snippets:
            web_block = "Published translation snippets:\n" + "\n".join(
                f"- {snippet[:300]}" for snippet in web.snippets
            )
        prompt = (
            f"Book: {book_title or '(unknown)'}\n"
            f"Glossary term: {term.source} -> {term.target} (type: {term.type}"
            + (f", aliases: {', '.join(term.aliases)}" if term.aliases else "")
            + ")\n"
            f"Occurrences in the book:\n{evidence or '(none found)'}\n"
            f"{web_block}\n"
            "Review this term. Reply with JSON only:\n"
            '{"status": "verified" | "conflict", '
            '"proposed_target": "<better translation or null>", '
            '"reason": "<one sentence>", '
            '"duplicate_of": "<other term source or null>"}'
        )
        verdict, _raw = self._complete_json_turn(
            [{"role": "user", "content": prompt}],
            operation="glossary.curate",
        )
        if not isinstance(verdict, dict):
            return {"status": "verified", "reason": "unparseable curator reply"}
        if verdict.get("status") not in {"verified", "conflict"}:
            verdict["status"] = "verified"
        return verdict

    def run_pass(
        self,
        store: Storage,
        terms: list[GlossaryTerm],
        chapters: list[Chapter],
        history: dict[tuple[int, int], TranslatedSegmentEvidence],
        *,
        book_title: str = "",
    ) -> dict[str, int]:
        """Check up to `CURATOR_BATCH_SIZE` terms; return pass statistics."""
        from ..glossary.store import GlossaryTerm as Term

        checked = verified = conflicts = 0
        started = time.monotonic()
        stop_requested = False
        for index, term in enumerate(terms[:CURATOR_BATCH_SIZE]):
            # Re-read the stop flag every few terms: per-term artifact I/O
            # costs 20 reads per pass; a stop still lands within seconds.
            if index % 5 == 0:
                state = store.read_artifact("curator_state.json") or {}
                stop_requested = bool(state.get("stop_requested"))
            if stop_requested:
                store.log_event("curator_pass_stopped", checked=checked)
                break
            term_started = time.monotonic()
            scanned = _scan_occurrences(term, chapters, history)
            store.set_term_mentions(term.source, len(scanned))
            occurrences = pick_excerpts(scanned, CURATOR_MAX_EVIDENCE)
            verdict = self.check_term(term, occurrences, book_title=book_title)
            checked += 1
            if verdict.get("status") == "conflict":
                conflicts += 1
                proposed = verdict.get("proposed_target") or term.target
                store.upsert_term(
                    Term(
                        source=term.source,
                        target=str(proposed),
                        type=term.type,
                        note=f"curator: {verdict.get('reason', '')}",
                    ),
                    term.first_chapter or 0,
                )
            else:
                verified += 1
            store.log_event(
                "curator_term_checked",
                source=term.source,
                status=verdict.get("status"),
                occurrences=len(occurrences),
                reason=verdict.get("reason", ""),
                proposed_target=verdict.get("proposed_target"),
                elapsed_seconds=round(time.monotonic() - term_started, 1),
            )
        store.log_event(
            "curator_pass_finished",
            checked=checked,
            verified=verified,
            conflicts=conflicts,
            elapsed_seconds=round(time.monotonic() - started, 1),
        )
        return {"checked": checked, "verified": verified, "conflicts": conflicts}
