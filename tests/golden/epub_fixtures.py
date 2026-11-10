"""Generate EPUB fixtures + oracle stage dumps for the Rust port.

Builds EPUBs with tests.sample_data builders, dumps each read stage
(package/toc/segments/layout/document) as JSON. Deterministic; run once and
commit outputs (fixtures are synthetic, no private books).
"""

from __future__ import annotations

import json
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "packages" / "core"))
sys.path.insert(0, str(ROOT / "packages" / "core" / "tests"))

from tests.sample_data import (  # noqa: E402
    write_cross_resource_toc_epub,
    write_degenerate_toc_epub,
    write_grouped_nav_epub,
    write_nested_toc_epub,
    write_sample_epub,
)
from wenyi_core.ingest import epub_package, epub_toc  # noqa: E402
from wenyi_core.ingest.epub_reader import read_epub  # noqa: E402
from wenyi_core.markup.segments import annotate_epub_resource  # noqa: E402

OUT = ROOT / "crates" / "wenyi-core" / "tests" / "fixtures" / "epub"


def dump(path: Path, name: str, payload: object) -> None:
    path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=1, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def build_fixtures() -> dict[str, Path]:
    OUT.mkdir(parents=True, exist_ok=True)
    specs = {
        "sample": lambda p: write_sample_epub(str(p)),
        "nested_toc": lambda p: write_nested_toc_epub(str(p)),
        "grouped_nav": lambda p: write_grouped_nav_epub(str(p)),
        "cross_resource": lambda p: write_cross_resource_toc_epub(str(p)),
        "degenerate": lambda p: write_degenerate_toc_epub(str(p)),
    }
    paths = {}
    for name, build in specs.items():
        epub = OUT / f"{name}.epub"
        build(epub)
        paths[name] = epub
    return paths


def dump_package(name: str, epub: Path) -> None:
    with zipfile.ZipFile(epub, "r") as zf:
        opf_path = epub_package._find_opf_path(zf)
        title, hrefs, toc_paths = epub_package._parse_opf(zf, opf_path)
        manifest_hrefs = epub_package._manifest_xhtml_hrefs(zf, opf_path)
        entries = epub_toc.parse_toc_entries(zf, toc_paths)
    dump(
        OUT / f"{name}.package.json",
        name,
        {
            "opf_path": opf_path,
            "title": title,
            "spine_hrefs": hrefs,
            "toc_paths": toc_paths,
            "manifest_xhtml_hrefs": manifest_hrefs,
            "toc_entries": entries,
        },
    )


def dump_resources(name: str, epub: Path) -> None:
    """Per-resource annotation dumps (title/segments/template/anchors)."""
    with zipfile.ZipFile(epub, "r") as zf:
        opf_path = epub_package._find_opf_path(zf)
        _, hrefs, toc_paths = epub_package._parse_opf(zf, opf_path)
        resources = []
        for index, href in enumerate(hrefs):
            html = epub_package._decode_markup(zf.read(href))
            title, segments, template = annotate_epub_resource(
                html, index, href, skip_navigation=href in toc_paths
            )
            resources.append(
                {
                    "index": index,
                    "href": href,
                    "title": title,
                    "segments": [s.model_dump() for s in segments],
                    "template": template,
                }
            )
    dump(OUT / f"{name}.resources.json", name, resources)


def dump_document(name: str, epub: Path) -> None:
    doc = read_epub(str(epub), "ja", "zh")
    dumped = doc.model_dump()
    # Normalize absolute source path for cross-machine comparison.
    dumped["source_path"] = "<abs>/" + Path(dumped["source_path"]).name
    dump(OUT / f"{name}.document.json", name, dumped)


def main() -> None:
    paths = build_fixtures()
    for name, epub in paths.items():
        dump_package(name, epub)
        dump_resources(name, epub)
        dump_document(name, epub)
    print(f"wrote fixtures for {sorted(paths)}")


if __name__ == "__main__":
    main()
