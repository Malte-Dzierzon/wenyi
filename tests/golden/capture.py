"""Capture golden snapshots from the pinned Python oracle.

Runs deterministic, offline pipeline behavior (FakeClient only, no model API)
over tests/golden/corpus/ and writes normalized JSON snapshots plus a
sha256sums.txt manifest. Run twice; outputs must be byte-identical.

Oracle pin: main@39068fcc (behavioral reference; see PLAN.md).
"""

from __future__ import annotations

import hashlib
import json
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "packages" / "core"))
sys.path.insert(0, str(ROOT / "packages" / "core" / "tests"))

CORPUS = Path(__file__).resolve().parent / "corpus"
SNAPSHOTS = Path(__file__).resolve().parent / "snapshots"

from tests.fake_llm import MeteredFakeClient, routing_handler  # noqa: E402
from tests.pipeline_fixtures import fake_pipeline_config  # noqa: E402
from tests.sample_data import write_sample_epub  # noqa: E402
from wenyi_core.config import Config  # noqa: E402
from wenyi_core.glossary.store import (  # noqa: E402
    GlossaryTerm,
    TermMatcher,
    normalize_match_key,
    source_matches_text,
)
from wenyi_core.ingest.segmenter import batch_segments  # noqa: E402
from wenyi_core.ingest.source_hash import source_sha256  # noqa: E402
from wenyi_core.ingest.srt_reader import parse_srt  # noqa: E402
from wenyi_core.ingest.tokens import count_tokens  # noqa: E402
from wenyi_core.pipeline.orchestrator import Orchestrator  # noqa: E402


def _write(name: str, payload: object) -> Path:
    SNAPSHOTS.mkdir(parents=True, exist_ok=True)
    path = SNAPSHOTS / name
    path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return path


def capture_token_counts() -> dict:
    texts = {
        "empty": "",
        "ascii": "The old lighthouse stood at the edge of the cliff.",
        "cjk": "吾輩は猫である。名前はまだ無い。",
        "mixed": "Keeper Elias Vance「灯台守」lit the lamp—again…",
        "whitespace": "  spaced\tout\n\nnewlines  ",
    }
    return {key: count_tokens(text) for key, text in texts.items()}


MATCH_CASES = [
    # (term, text, note)
    ("Elias Vance", "keeper Elias Vance lit the lamp", "latin full name"),
    ("Ann", "baker Anna Berger prepared bread", "no match inside longer word"),
    ("Ann", "Her daughter Ann helped shape", "standalone short name"),
    ("猫", "吾輩は猫である", "cjk single char substring"),
    ("名前", "名前はまだ無い", "cjk word"),
    ("ｌａｍｐ", "lit the lamp again", "full-width normalization"),
    ("LAMP", "lit the lamp again", "unicode case folding"),
    ("storm,", "Another storm coming", "punctuation-insensitive key"),
    ("", "anything", "empty term never matches"),
    ("lamp", "", "empty text never matches"),
]


def capture_matcher() -> dict:
    per_case = []
    for term, text, note in MATCH_CASES:
        matcher = TermMatcher([term])
        per_case.append(
            {
                "term": term,
                "text": text,
                "note": note,
                "normalized_key": normalize_match_key(term),
                "matches": source_matches_text(term, text),
                "first_hit": matcher.first_hit(text),
            }
        )
    # Alias + overlap behavior through a real term.
    term = GlossaryTerm(
        source="Elias Vance",
        target="埃利亚斯·万斯",
        type="person",
        aliases=["Elias", "Keeper Vance"],
    )
    alias_matcher = TermMatcher(term)
    alias_cases = {
        "alias_hit": alias_matcher.first_hit("Elias tightened his coat"),
        "source_hit": alias_matcher.first_hit("Keeper Vance watched the ships"),
        "miss": alias_matcher.first_hit("The baker prepared bread"),
    }
    return {"cases": per_case, "aliases": alias_cases}


def capture_source_hashes(tmp: Path) -> dict:
    return {
        name: source_sha256(str(CORPUS / name))
        for name in ("two_chapters.txt", "headings.md", "three_cues.srt")
    } | {"tmp_copy_differs": source_sha256(str(tmp)) != ""}


def capture_segmentation() -> dict:
    from wenyi_core.ingest.text_reader import read_text

    chapters = read_text(str(CORPUS / "two_chapters.txt"), "en", "zh").chapters
    dumped = [
        {
            "index": index,
            "title": chapter.title,
            "segments": [
                {
                    "index": segment.index,
                    "kind": segment.kind,
                    "chars": len(segment.source),
                    "tokens": count_tokens(segment.source),
                }
                for segment in chapter.text_segments
            ],
        }
        for index, chapter in enumerate(chapters)
    ]
    first = chapters[0].text_segments
    batches = [[segment.index for segment in batch] for batch in batch_segments(first, 1800)]
    return {"chapters": dumped, "first_chapter_batches_1800": batches}


def capture_srt() -> dict:
    cues = parse_srt(str(CORPUS / "three_cues.srt"))
    return [
        {
            "index": cue.index,
            "timestamp": cue.timestamp,
            "chars": len(cue.text),
            "tokens": count_tokens(cue.text),
        }
        for cue in cues
    ]


def capture_epub(tmp: Path) -> dict:
    from wenyi_core.ingest.epub_reader import read_epub

    epub_path = tmp / "sample.epub"
    write_sample_epub(str(epub_path))
    doc = read_epub(str(epub_path), "ja", "zh")
    return {
        "title": doc.title,
        "chapters": [
            {
                "title": chapter.title,
                "segments": len(chapter.text_segments),
                "chars": sum(len(s.source) for s in chapter.text_segments),
            }
            for chapter in doc.chapters
        ],
    }


def capture_translate(tmp: Path) -> dict:
    """Minimal end-to-end TXT run (prepare + translate, no review/autofix)."""

    src = tmp / "novel.txt"
    src.write_text((CORPUS / "two_chapters.txt").read_text(encoding="utf-8"))
    cfg = fake_pipeline_config(str(tmp / "state"))
    cfg.pipeline.review = False
    cfg.pipeline.review_autofix = False
    client = MeteredFakeClient(handler=routing_handler)
    orch = Orchestrator(cfg, client=client)
    store = orch.run(str(src))
    manifest = store.load_manifest()
    chapters = []
    for row in manifest.get("chapters", []):
        chapter = store.load_chapter(row["index"])
        chapters.append(
            {
                "index": row["index"],
                "title": row.get("title"),
                "segments": [
                    {
                        "index": segment.index,
                        "target": segment.target,
                        "has_polish_baseline": segment.target_before_polish is not None,
                    }
                    for segment in chapter.text_segments
                ],
            }
        )
    usage = store.load_usage() or {}
    return {
        "chapters": chapters,
        "translated_segments": sum(
            1
            for chapter in chapters
            for segment in chapter["segments"]
            if segment["target"] is not None
        ),
        "usage_totals": usage.get("totals", {}),
        "events": [
            {k: v for k, v in event.items() if k not in {"ts", "timestamp"}}
            for event in (store.list_events() or [])
            if event.get("event")
            in {
                "translate_run_started",
                "translate_run_finished",
                "chapter_translated",
            }
        ],
    }


def capture_config() -> dict:
    cfg = Config()
    return {
        "source_lang": cfg.source_lang,
        "target_lang": cfg.target_lang,
        "segment": cfg.segment.model_dump(),
        "pipeline_keys": sorted(cfg.pipeline.model_dump().keys()),
        "output_keys": sorted(cfg.output.model_dump().keys()),
    }


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="wenyi-golden-") as tmpdir:
        tmp = Path(tmpdir)
        (tmp / "probe.txt").write_text("probe", encoding="utf-8")
        snapshots = {
            "tokens.json": capture_token_counts(),
            "matcher.json": capture_matcher(),
            "source_hashes.json": capture_source_hashes(tmp / "probe.txt"),
            "segmentation.json": capture_segmentation(),
            "srt.json": capture_srt(),
            "epub.json": capture_epub(tmp),
            "config_defaults.json": capture_config(),
            "translate_txt.json": capture_translate(tmp),
        }
    written = [_write(name, payload) for name, payload in snapshots.items()]
    manifest = SNAPSHOTS / "sha256sums.txt"
    lines = []
    for path in sorted(written):
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        lines.append(f"{digest}  {path.name}\n")
    manifest.write_text("".join(lines), encoding="utf-8")
    print(f"wrote {len(written)} snapshots + sha256sums.txt")


if __name__ == "__main__":
    main()
