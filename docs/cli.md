# Engine behavior contract

[简体中文](zh/cli.md) · [README](../README.md) · [Pipeline](pipeline.md) · [Configuration](configuration.md)

> The `wenyi` command-line product was removed during the Rust-rewrite cleanup
> (see Git history). This file keeps its **behavioral specification**: formats,
> state layout, workflow stages, and invariants that the Desktop app and the
> future Rust engine must honor. The filename is retained for link stability.
> Command names below (`parse`, `translate`, `review`, `assemble`, …) describe
> engine operations, not an existing CLI. Former flags such as `--format` or
> `--out` are retired; the same choices are made in the Desktop UI or through
> project configuration.

Engine entry: `packages/core/wenyi_core/pipeline/orchestrator.py`. Configuration
defaults come from `config.yaml` (`packages/core/wenyi_core/config.py`).

## State layout

All book targets, including the default `zh`, use:

```text
<state_dir>/<slug>/targets/<target-language>/
  manifest.json          # committed last; source identity and run metadata
  chapters/              # chapter JSON with source/target segments
  glossary.db            # SQLite glossary
  usage.json             # cumulative token usage
  timing.json            # cumulative execution time and invocation records
  events.jsonl           # append-only stage and retry events
  reviews/               # timestamped Review runs and autofix journals
```

Subtitles use a separate tree:

```text
<state_dir>/srt/<slug>/targets/<target-language>/
  manifest.json
  cues.jsonl             # index, timestamp, source, target, status
  batches/               # raw model results for resume
  usage.json
  timing.json
  events.jsonl
```

There is no `glossary.db` or `reviews/` tree for subtitles. `source_sha256`
binds state to input content; a different file with the same name cannot resume.
See [Configuration — paths](configuration.md#segmentation-honorifics-and-paths).

## Input and output

Input formats: EPUB, FB2, TXT, Markdown, HTML, PDF, DOCX, and SRT.

- Markdown headings (`#` through `######`, followed by whitespace and title
  text) start chapters. Original levels are stored and preserved in EPUB body
  headings, generated navigation, and DOCX outline styles.
- Default book output is `output/<book-name>.<target>.<ext>`; the bilingual
  edition is `output/<book-name>.<target>-bi.<ext>`.
- Default export format: BabelDOC PDF state → `pdf`, DOCX input → `docx`, other
  books (including MinerU PDF state) → `epub`. An explicit format always wins.
  PDF defaults follow the saved backend even if `pipeline.pdf_backend` changed.
- EPUB output attempts to write translations back into the original XHTML
  templates, preserving styles, images, TOC, and anchors.
- The bilingual edition shows translation and source together; source styling
  and order are controlled by `output.bilingual_*`.
- EPUB output includes an “About this translation” page by default
  (`output.about_page: false` disables it).

### PDF

PDF input and output are experimental. The default input backend is MinerU and
requires `MINERU_API_KEY` for the first conversion. The optional BabelDOC bridge
is an external AGPL service accessed over HTTP only; Wenyi never imports it.
BabelDOC is for PDFs with a text layer; scanned pages should use MinerU or OCR.
BabelDOC state defaults to PDF output and preserves `meta.babeldoc_id` for
fillback. See [Configuration — pipeline](configuration.md#pipeline) for bridge
settings and [Pipeline](pipeline.md) for review behavior.

PDF output uses WeasyPrint by default (`pdf-output` optional dependency).
`fpdf2` (`pdf-output-lite`) is a lighter fallback with limited HTML/CSS support;
set `TRANS_NOVEL_PDF_FONT` if no CJK system font is discoverable.

### DOCX

DOCX books use the full book pipeline (glossary, polish, review, resume). The
reader preserves heading styles and outline levels, simple tables, list groups,
and common run/paragraph formatting. Uniform runs export without an extra model
call; mixed runs align meaningful spans after translation and inherit source
attributes. Translated Chinese uses Song; untranslated source and bilingual
source lines do not force it. Default output is `output/<stem>.<target>.docx`.

### SRT

`.srt` files take the independent subtitle path in `wenyi_core.srt`:

- sliding windows of 20 cues with overlap 10, up to 100 concurrent strong-tier
  calls;
- no glossary, polishing, or whole-book review;
- book-only options are ignored or rejected;
- default output `output/<stem>.<target>.srt`; bilingual `.srt` optional.

Resume by selecting the same source again. Cached batches under
`state/srt/<slug>/targets/<target-language>/batches/` are skipped.

## Workflow stages

- **`parse`** — list source chapters without model calls. Reports zero-based
  chapter indices, source titles, and paragraph counts; saves
  `parsed_document.json` under the target-language state directory. It does not
  require LLM credentials, detect language, generate digests/style/terms, or
  mark translation state initialized. `language.source: auto` stays unresolved
  until preparation. PDF parsing still needs the configured service; SRT uses
  its own path.
- **`prepare`** — parse, detect language, generate chapter digests when
  `book_understanding` is enabled, build the style guide and initial glossary,
  then generate the whole-book synopsis. No body text is translated.
- **`translate`** — run the complete workflow, translate one chapter, or resume
  pending work. Completed batches and chapters are skipped. Polishing and review
  follow `pipeline.polish` and `pipeline.review`.
- **`review`** — check the complete translated book against the final glossary.
  Review is read-only by default; `pipeline.review_autofix` enables the separate
  publication stage. Matching content/configuration/glossary fingerprints reuse
  completed results or resume unfinished work.
- **`assemble`** — rebuild output from persisted state without model calls. It
  freezes a consistent snapshot, releases the state lock, and renders from that
  snapshot. Run again to include batches completed afterward.
- **`report`** — summarize translation state and the latest Review without
  changing text.
- **Glossary management** — list terms, inspect conflicts, and resolve terms.
  The glossary constrains later translation but does not rewrite completed
  occurrences automatically.

Stage options are configuration-driven; there is no CLI to pass overrides.
Project creation chooses standard or precision mode; precision requires
`pipeline.polish: true`. See [Pipeline — precision](pipeline.md#best-of-three-precision-translation).

## Usage, timing, and events

Each target directory stores cumulative token usage in `usage.json` and appends
stage events and retries to `events.jsonl`. Review runs also record a run-local
usage delta; it is merged into the cumulative ledger exactly once.

The run clock covers the entire current workflow, including parsing, model
waits, translation, polishing, review, and export. It does not reset at stage,
chapter, or round boundaries. Concurrent model requests contribute wall time,
not the sum of request durations. `timing.json` stores `total_seconds` and a
`runs` list with invocation IDs, operations, timestamps, durations, and
completion statuses. Inspection and report regeneration add no time.

After state initialization or validation, failures and normal Ctrl+C exits also
save the invocation's elapsed time. Timing is committed atomically under its own
lock, independently of usage. A forced kill or a failure before initialization
cannot save the current duration.

## Resume and consistency guarantees

- Every completed batch is written to the state directory immediately. Re-running
  `translate` skips completed batches and fills missing work.
- With polishing, chapter JSON keeps the translation-stage text in
  `target_before_polish` and the polished text in `target`.
- Changing polishing settings does not rerun completed batches. Review is
  different: every independent review invocation rechecks the complete book and
  creates a new timestamped run unless a matching unfinished run can resume.
- Initialization commits `manifest.json` last. Publication writes a recoverable
  index before changing formal `target` values.
- Export reads a consistent snapshot of the manifest and chapter data.
- On Windows, local JSON state and artifact access use extended-length paths;
  artifact keys remain relative and `/`-separated.

See [Pipeline](pipeline.md) for review internals, [Configuration](configuration.md)
for every setting, and [Architecture](architecture.md) for module ownership.
