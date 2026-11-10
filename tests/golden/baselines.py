"""Feasible B1-B12 baselines against the pinned Python oracle (Slice 0).

Measures what the Python implementation can demonstrate today; Rust slices
must meet or beat these. All offline (FakeClient), synthetic data only.
Writes tests/golden/snapshots/baselines.json (NOT hashed: timings vary).
"""

from __future__ import annotations

import json
import resource
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "packages" / "core"))
sys.path.insert(0, str(ROOT / "packages" / "core" / "tests"))

from tests.fake_llm import MeteredFakeClient, routing_handler  # noqa: E402
from tests.pipeline_fixtures import fake_pipeline_config  # noqa: E402
from wenyi_core.glossary.store import GlossaryStore, GlossaryTerm  # noqa: E402
from wenyi_core.ingest.tokens import count_tokens  # noqa: E402
from wenyi_core.pipeline.curator import _scan_occurrences  # noqa: E402
from wenyi_core.pipeline.orchestrator import Orchestrator  # noqa: E402

CORPUS = Path(__file__).resolve().parent / "corpus"
SNAPSHOTS = Path(__file__).resolve().parent / "snapshots"


def rss_mb() -> float:
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024


def bench_translate() -> dict:
    """B6/B9-shape: end-to-end TXT run wall time + peak RSS + usage calls."""
    with tempfile.TemporaryDirectory(prefix="wenyi-bench-") as tmpdir:
        tmp = Path(tmpdir)
        src = tmp / "novel.txt"
        # ~200 paragraphs of mixed EN/CJK to make timing meaningful.
        para_en = "The old lighthouse stood at the edge of the cliff. "
        para_cjk = "吾輩は猫である。名前はまだ無い。"
        src.write_text(
            "# Chapter One\n\n"
            + "\n\n".join(para_en * 4 if i % 2 == 0 else para_cjk * 8 for i in range(200))
            + "\n",
            encoding="utf-8",
        )
        cfg = fake_pipeline_config(str(tmp / "state"))
        cfg.pipeline.review = False
        cfg.pipeline.review_autofix = False
        client = MeteredFakeClient(handler=routing_handler)
        orch = Orchestrator(cfg, client=client)
        start = time.perf_counter()
        store = orch.run(str(src))
        wall = time.perf_counter() - start
        usage = store.load_usage() or {}
        chapters = len(store.load_manifest().get("chapters", []))
        return {
            "wall_seconds": round(wall, 3),
            "peak_rss_mb": round(rss_mb(), 1),
            "chapters": chapters,
            "usage_calls": (usage.get("totals", {}) or {}).get("calls"),
            "total_tokens": (usage.get("totals", {}) or {}).get("total_tokens"),
        }


def bench_matcher_scan() -> dict:
    """B7-shape: curator-style full scan cost vs glossary size (the O(T*C) baseline)."""
    from wenyi_core.ingest.models import Chapter, Segment

    chapters = [
        Chapter(
            index=i,
            title=f"ch{i}",
            segments=[
                Segment(
                    index=j,
                    source="Elias Vance and Anna Berger walked past the lighthouse. "
                    "吾輩は猫である。",
                    kind="text",
                )
                for j in range(50)
            ],
        )
        for i in range(20)
    ]  # 1000 segments
    results = {}
    for n_terms in (10, 100, 500):
        terms = [
            GlossaryTerm(source=f"Term{i}", target=f"译{i}", type="person") for i in range(n_terms)
        ]
        terms.extend(
            [
                GlossaryTerm(source="Elias Vance", target="埃", type="person"),
                GlossaryTerm(source="猫", target="猫", type="term"),
            ]
        )
        start = time.perf_counter()
        hits = sum(len(_scan_occurrences(term, chapters, None)) for term in terms)
        wall = time.perf_counter() - start
        results[f"terms_{n_terms}"] = {
            "wall_seconds": round(wall, 3),
            "hits": hits,
        }
    return {"segments": 1000, "by_glossary_size": results}


def bench_tokenizer() -> dict:
    """Tokenizer throughput on multilingual text (parity gate input)."""
    text = ("The old lighthouse 吾輩は猫である\n" * 2000)[:100_000]
    start = time.perf_counter()
    n = count_tokens(text)
    wall = time.perf_counter() - start
    return {"chars": len(text), "tokens": n, "wall_seconds": round(wall, 3)}


def bench_glossary_store() -> dict:
    """B3-shape: glossary round-trip latency at moderate size."""
    with tempfile.TemporaryDirectory(prefix="wenyi-bench-gloss-") as tmpdir:
        store = GlossaryStore(str(Path(tmpdir) / "glossary.db"))
        start = time.perf_counter()
        for i in range(1000):
            store.upsert_term(GlossaryTerm(source=f"Term{i}", target=f"译{i}", type="person"))
        insert_wall = time.perf_counter() - start
        start = time.perf_counter()
        terms = store.all_terms()
        read_wall = time.perf_counter() - start
        return {
            "insert_1000_wall": round(insert_wall, 3),
            "read_all_wall": round(read_wall, 3),
            "terms": len(terms),
        }


def main() -> None:
    baselines = {
        "oracle": "main@39068fcc",
        "notes": {
            "B1_cold_start": "not measurable on oracle (no shipped binary yet); "
            "record at Rust Slice 7",
            "B2_project_list": "N/A to file oracle; Rust must show no N+1 scans",
            "B5_progress_rate": "Python hub coalesces at 4Hz by design; "
            "Rust keeps <=4Hz coarse events",
            "B10_provider": "covered by fake-HTTP tests at Rust Slice 3",
            "B11_export": "covered by round-trip goldens at Rust Slice 2+",
            "B12_size_build": "record at first Rust build (Slice 1)",
        },
        "B6_translate_200para": bench_translate(),
        "B7_matcher_scan": bench_matcher_scan(),
        "tokenizer_throughput": bench_tokenizer(),
        "B3_glossary_store": bench_glossary_store(),
    }
    SNAPSHOTS.mkdir(parents=True, exist_ok=True)
    (SNAPSHOTS / "baselines.json").write_text(
        json.dumps(baselines, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(json.dumps(baselines, ensure_ascii=False, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
