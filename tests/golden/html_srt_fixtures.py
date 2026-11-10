"""Capture oracle HTML/SRT reader dumps as golden fixtures for the Rust port."""

from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "packages" / "core"))

from wenyi_core.ingest.html_reader import read_html  # noqa: E402
from wenyi_core.ingest.srt_reader import parse_srt  # noqa: E402

OUT = ROOT / "crates" / "wenyi-core" / "tests" / "fixtures"
HTML_FIXTURE = """\
<html><head><title>Book</title><style>p { color: red; }</style></head>
<body>
<p>Front matter before any heading.</p>
<h1>Chapter One</h1>
<p>First paragraph with <b>bold</b> and <img src="pic.png"/> image.</p>
<blockquote>Quoted text.</blockquote>
<h2>Section 1.1</h2>
<h3>Deep dive</h3>
<p>Nested heading run body.</p>
<ul><li>First item</li><li>Second item</li></ul>
<h1>Chapter Two</h1>
<p>Second chapter body with ruby: <ruby>漢<rt>かん</rt></ruby>字.</p>
<div class="cover"></div>
</body></html>
"""


def dump(path: Path, payload: object) -> None:
    path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=1, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def main() -> None:
    html_dir = OUT / "html"
    html_dir.mkdir(parents=True, exist_ok=True)
    source = html_dir / "sample.html"
    source.write_text(HTML_FIXTURE, encoding="utf-8")
    doc = read_html(str(source), "ja", "zh")
    dumped = doc.model_dump()
    dumped["source_path"] = "<abs>/sample.html"
    # Templates are serializer-specific; compare structurally in Rust.
    templates = [c.pop("template") for c in dumped["chapters"]]
    dump(html_dir / "sample.document.json", dumped)
    dump(html_dir / "sample.templates.json", templates)

    srt_dir = OUT / "srt"
    srt_dir.mkdir(parents=True, exist_ok=True)
    cues = parse_srt(str(ROOT / "tests" / "golden" / "corpus" / "three_cues.srt"))
    dump(
        srt_dir / "three_cues.json",
        [{"index": c.index, "timestamp": c.timestamp, "text": c.text} for c in cues],
    )
    print("wrote html + srt fixtures")


if __name__ == "__main__":
    main()
