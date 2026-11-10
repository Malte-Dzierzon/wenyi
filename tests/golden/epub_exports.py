"""Capture oracle EPUB exports as golden fixtures for the Rust assembly port.

For each fixture EPUB: stage via load_document + FileStorage, fill
deterministic targets (T{chapter}-{segment}), add translated TOC titles,
then export through the real `assemble()` path in four modes:
mono, bilingual (target_first), bilingual (source_first), bilingual +
preserve_source_style. Dumps each export's XHTML/OPF/NCX/NAV members as JSON
so Rust parity tests can compare member-by-member.
"""

from __future__ import annotations

import json
import sys
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "packages" / "core"))
sys.path.insert(0, str(ROOT / "packages" / "core" / "tests"))

from wenyi_core.assemble.writer import assemble  # noqa: E402
from wenyi_core.ingest.segmenter import load_document  # noqa: E402
from wenyi_core.storage.file import FileStorage  # noqa: E402

FIXTURES = ROOT / "crates" / "wenyi-core" / "tests" / "fixtures" / "epub"
NAMES = ["sample", "nested_toc", "grouped_nav", "cross_resource", "degenerate"]

MODES = {
    "mono": {},
    "bi": {"bilingual": True},
    "bi_src": {"bilingual": True, "order": "source_first"},
    "bi_style": {"bilingual": True, "preserve_source_style": True},
}


def export_members(source: Path, mode: str) -> dict[str, str]:
    with tempfile.TemporaryDirectory() as directory:
        document = load_document(str(source), "ja", "zh")
        store = FileStorage(str(Path(directory) / "state"))
        manifest = store.init_from_document(document)
        for chapter_meta in manifest["chapters"]:
            chapter = store.load_chapter(chapter_meta["index"])
            for segment in chapter.segments:
                segment.target = (
                    f"T{chapter_meta['index']}-{segment.index} {segment.source}"
                )
            store.save_chapter(chapter)
        for entry in manifest["meta"].get("toc_entries", []):
            # ASCII titles: the oracle mojibakes CJK TOC rewrites in
            # declaration-less NAV docs (UnicodeDammit cp949 mis-detect);
            # the Rust port deliberately does not reproduce that bug.
            entry["title_translated"] = f"TR-{entry.get('title', '')}"
        store.save_manifest(manifest)
        out = str(Path(directory) / "out.epub")
        assemble(store, str(source), out_path=out, out_format="epub",
                 about_page=False, **MODES[mode])
        with zipfile.ZipFile(out) as archive:
            members = {}
            for name in archive.namelist():
                low = name.lower()
                if low.endswith((".xhtml", ".html", ".htm", ".opf", ".ncx")):
                    members[name] = archive.read(name).decode("utf-8")
            return {"namelist": archive.namelist(), "members": members}


def main() -> None:
    for name in NAMES:
        source = FIXTURES / f"{name}.epub"
        payload = {mode: export_members(source, mode) for mode in MODES}
        (FIXTURES / f"{name}.exports.json").write_text(
            json.dumps(payload, ensure_ascii=False, indent=1, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        print(f"wrote {name}.exports.json")


if __name__ == "__main__":
    main()
