# Headless behavior specification

[简体中文](zh/cli.md) · [README](../README.md)

> The `wenyi` command-line product was removed during the Rust-rewrite cleanup
> (see git history). This document preserves its **behavioral specification**:
> formats, state layout, workflow stages, and options that the Desktop app and
> the future Rust engine must honor. Command names below (`parse`, `translate`,
> `review`, `assemble`, …) describe engine operations, not an existing CLI.

Engine entry: `packages/core/wenyi_core/pipeline/orchestrator.py`. Configuration
defaults come from `config.yaml` (`packages/core/wenyi_core/config.py`).

## Model routing inspection

The engine exposes route preview and credential checks without requests
(`models list` / `explain` / `check` semantics), plus a live connectivity probe
(`ping` semantics: one short request per reachable profile, no retries, failures
by category). See [configuration](configuration.md#models-and-operation-routing).

## Multilingual translation (experimental)

Each run selects one direction (`language.source`/`target`); `source: auto` detects
with a model call unless a known ISO code is given. Outputs follow
`output/<book>.<target>.<ext>` and `output/<book>.<target>-bi.<ext>` for bilingual.
Explicit `--out` names and their `-bi` derivatives retain their existing behavior.

## Input and output

- Input formats: EPUB, FB2, TXT, Markdown, HTML, PDF, DOCX, and SRT.
- For Markdown input, headings starting with `#` through `######`, followed by whitespace and title text, start new chapters. Their original levels are recorded during import; plain-text contents lists with page numbers remain body text.
- Markdown heading levels are preserved in EPUB body headings, the generated EPUB navigation tree, and DOCX outline styles. Navigation nests each heading under the nearest preceding heading with a smaller level number; skipped levels do not create synthetic entries. Existing state with correct chapter divisions and stored heading levels only needs a fresh export, not retranslation.
- Default book output: a monolingual `<book-name>.zh.epub` under the source file's `output/` directory (`.docx` inputs default to `<book-name>.zh.docx`, and BabelDOC PDF state defaults to `<book-name>.zh.pdf`). The bilingual `*.zh-bi.*` edition is optional.
- `--format epub|txt|html|markdown|pdf|docx`: export the selected format for book inputs. When omitted, BabelDOC PDF state → `pdf`, `.docx` → `docx`, and other books (including MinerU PDF state) → `epub`. An explicit format always takes precedence; PDF defaults follow the saved backend, even if the current `pdf_backend` setting has changed. This flag does not apply to SRT.
- For EPUB input, Wenyi attempts to write translated text back into the original XHTML templates while preserving styles, images, the table of contents, and anchors.
- The bilingual edition displays the translation and source text together. The source is visually subdued by default; set `output.bilingual_preserve_source_style: true` to inherit the book's normal text style. Their order is controlled by `output.bilingual_order`.
- Untranslated EPUB paragraphs appear once as the original text, retaining ruby readings and links. This also applies to partially translated chapters; bilingual ordering and source styling do not add a second copy.
- EPUB output includes an “About this translation” page by default. Set `output.about_page: false` to disable it.
- Book runtime data is stored under `state/<book>/targets/<target-language>/`, including chapter intermediates, the SQLite glossary, usage data, and reports. Subtitle runs use a separate tree under `state/srt/` (see [SRT subtitles](#srt-subtitles)).

### Experimental PDF support

PDF input and PDF output are both experimental.

#### PDF input

Default backend is MinerU. For layout-preserving export, use the external
**BabelDOC bridge** (AGPL, separate repo/process, HTTP only):

1. Install/start `wenyi-babeldoc-bridge` (default `http://127.0.0.1:8765`)
2. In `config.yaml`:

```yaml
pipeline:
  pdf_backend: babeldoc
  babeldoc_bridge_url: http://127.0.0.1:8765
  # babeldoc_pages: "15"   # optional, 1-based
```

3. Translating `book.pdf` automatically exports PDF through bridge `/fillback`. Later, assembling `book.pdf` also defaults to PDF for that saved BabelDOC state. Use an explicit format to select another format.
   The fillback PDF omits BabelDOC layout overlay boxes and role labels
   such as ``plain text`` / ``title`` by default.
   The bridge freezes the post-extraction IL as a durable session snapshot. It can
   restart and lazily restore the same session without rerunning layout analysis,
   provided its session directory and exact Python/BabelDOC versions are retained.
   Set `WENYI_BABELDOC_STATE_DIR` to a persistent directory for long translations;
   the default system temporary directory may be cleaned after a reboot. Wenyi never
   imports babeldoc. Chapters are split from the PDF outline (bookmarks); segments
   keep `meta.babeldoc_id` for fillback. Books without outlines fall back to one chapter.

BabelDOC is intended for PDFs with an extractable text layer. Before contacting the
bridge, Wenyi checks the selected pages and stops with a suggestion to use the default
MinerU backend or run OCR first when it finds only scanned images and no text layer.

The first MinerU PDF import requires `MINERU_API_KEY`. MinerU's converted HTML is saved at
`state/<book>/targets/<target-language>/source/<source-sha256>/converted.html`. The content-addressed
directory prevents an interrupted run from reusing another PDF's conversion.
Later runs reuse this file, and it may be corrected manually before resuming.

#### PDF output

WeasyPrint is the default PDF engine (optional `pdf-output` dependency).
For a lightweight cross-platform engine without system rendering libraries,
use `fpdf2` (optional `pdf-output-lite` dependency, `--pdf-engine fpdf2`).

`fpdf2` supports basic layout and images, but only a limited HTML/CSS subset.
Images mixed with text are placed as separate blocks. It uses a discoverable
CJK system font; if none is found, set `TRANS_NOVEL_PDF_FONT` to a TTF, OTF, or
TTC font file. This option also works on Windows.

## DOCX (Word)

DOCX books use the full book Orchestrator (glossary, polish, review, resume under `state/<slug>/targets/<target-language>/`).

**Structure**

- Paragraphs and heading styles (`Heading 1`–`9` / outline levels); level-1 headings start chapters.
- Simple tables are rebuilt cell-by-cell (no merged cells / nested tables in v1).
- Word automatic lists (`numPr`) become List Number / List Bullet groups (restart per source list id).
- Documents without the optional numbering part are also accepted; ordinary paragraphs do not require list definitions.
- Contents-style lines that already include a visible prefix such as `1. Title` are **not** auto-numbered again (avoids double numbering).

**Styles**

- Keeps bold / italic / underline / color / size, paragraph alignment, and shading.
- Uniform runs: apply on export with **no** extra model call.
- Mixed runs: after translate, each meaningful span is positioned alone (EPUB-annotation-style markers); bold/color and other attrs are **inherited from the source item**. Failed spans fall back proportionally without discarding the whole paragraph.
- Font/size-only run splits are ignored for alignment (noise).
- Translated Chinese uses **Song (宋体)**; untranslated source text and bilingual source lines do **not** force Song.
- Default Heading theme blue is neutralized unless the source set an explicit color.

**Output**

- Default: `output/<stem>.zh.docx` (Navigation pane via heading outline). Override with `--format epub` (etc.).

## SRT subtitles

`.srt` files route automatically. The subtitle path is intentionally
lighter than the book pipeline:

- sliding windows of 20 cues with overlap 10, up to 100 concurrent strong-tier calls;
- no glossary, polishing, or whole-book review;
- chapter/polish/review/format options are ignored or rejected where they do not apply;
- monolingual `output/<stem>.zh.srt` by default; bilingual `.zh-bi.srt` optional.

Resume by running the same source file again. Cached batches under
`state/srt/<slug>/targets/<target-language>/batches/` are skipped. Layout:

```text
state/srt/<slug>/targets/<target-language>/
  manifest.json    # source identity, cue counts, window settings
  cues.jsonl       # one cue per line: index, timestamp, source, target, status
  batches/         # raw model results for resume
  usage.json       # cumulative token usage across resumes
  timing.json      # cumulative execution time and individual invocation durations
  events.jsonl     # run events and LLM retry observations
```

There is no `glossary.db` or `reviews/` tree for subtitles. Package code lives in
`wenyi_core.srt` (store + translate), with I/O in `ingest.srt_reader` and
`assemble.srt_writer`.

## Usage and event logs

Each target directory stores cumulative token usage in `usage.json` and appends stage events and retries to `events.jsonl`. Review directories also record session usage; each increment is merged into the cumulative ledger once.

The run clock measures the entire current workflow, including parsing, model
waits, translation, polishing, review and export. Switching stages, chapters or review
rounds does not reset it; completing one stage does not stop it while subsequent work
is pending. Concurrent model requests contribute wall time, not the sum of request durations.

After `prepare`, `translate` (including single-chapter and SRT), `review` or `assemble`,
the run records the last run's duration and cumulative execution time. Each target's
`timing.json` stores `total_seconds` and a `runs` list with invocation IDs, operations,
timestamps, durations and completion statuses. Repeating an operation adds only that
invocation's execution time, excluding downtime between runs; nested pipeline stages
are counted once. Separately launched operations contribute their own durations, even
when they overlap. Book timing can also be inspected via status/report operations;
inspection and report regeneration do not add time.

Once book state has been initialized or validated, failures and normal Ctrl+C exits
also save the invocation's elapsed time. Timing is committed atomically under its own
lock, independently of token usage. Older runs have no timing history to recover;
time is accumulated from this version onward. A forced kill or a failure before state
initialization cannot save the current invocation's duration.

Manifests bind input content with `source_sha256`. A different file with the same name, or state without a valid hash, cannot resume; create new translation state.

## Workflow stages

- `parse`: list the source chapters without model calls. Reports zero-based chapter indices, source titles and paragraph counts, and saves the source document in `parsed_document.json` under the target-language state directory. It does not require LLM credentials, detect language with a model, generate digests/style/terms, or mark translation state initialized. `language.source: auto` remains unresolved until preparation. PDF parsing still needs the configured MinerU/BabelDOC service. SRT uses the independent subtitle workflow, not this book operation.
- `translate` / `prepare`: run the complete workflow, translate one chapter, or prepare without translating. `prepare` parses the book, detects its language, generates chapter digests when enabled, builds the style guide and initial glossary, and then generates the whole-book synopsis when enabled, without translating any body text.
- Stage overrides: `--polish`/`--no-polish`, `--review`/`--no-review`, `--chapter N`, `--format`, `--bilingual`/`--no-mono --bilingual`, `--translation-mode best_of_three --polish`.

Matching cached parsing is reused, including PDF conversion; changed content or parsing options invalidate it. An existing initialized run rejects different source content and keeps saved translations, analysis and usage unchanged.

## Best-of-three precision translation

Precision mode (`pipeline.translation_mode: best_of_three` with `pipeline.polish: true`)
runs three independent drafts followed by one source-first synthesis. Precision with
polish disabled is rejected; the option does not apply to SRT. When resuming, keep the
same mode/configuration to reuse precision checkpoints. Existing translations are not
automatically reprocessed. See [precision workflow](pipeline.md#best-of-three-precision-translation) for
resume and cost/quality limitations. A normal batch uses four model calls versus two for
standard translation with polishing, with the same routes and output limits.
The final synthesis is structurally validated, not independently accuracy-judged.
In comparisons, `target_before_polish` is the first initial draft, not a selected
accurate draft. Quality improvement is not guaranteed.

## Interrupting and resuming

Every completed batch is written to the state directory. Resume by running the same source file again.

Changing polishing settings does not automatically rerun translation batches that
are already complete. Review is different: every `review` invocation rechecks the
complete translated book and creates a new timestamped Review run. Runs are
read-only by default; autofix may publish their final revisions.
Start new state only when intentionally wanting a fresh translation.

Language policies are built into the source code. When they change or saved state lacks a policy identity, `prepare`/`translate` automatically rebuilds affected analysis before pending work resumes; this may call models. Completed translations remain saved. Font and other export-only changes need a fresh export only. See [built-in language policies](configuration.md#built-in-language-policies) for book/SRT behavior and developer diagnostics.

## Independent stages and glossary management

- `review` / `review --autofix`: check the complete translated book using the final glossary.
- `glossary list` / `glossary conflicts` / `glossary resolve <source> <translation>`: inspect and resolve terms.
- `report`: summarize the current translation and latest Review result without modifying text.
- `assemble`: rebuild output from existing state without calling the model again. If translation is still running elsewhere, export uses a consistent snapshot of the batches already persisted when the operation starts; run it again to include batches completed afterward.

`review` checks the complete translated book using the final glossary. Its
unchanged initial Reviewer runs over contiguous chunks concurrently; candidates
can then enter a bounded evidence loop, and contradictory cross-chunk consistency
suggestions can receive a final recommendation. Confirmed issues may generate
provisional full-segment replacements in a run-local shadow translation. Every
Fixer in a round reads the same immutable snapshot; the next whole-book pass
blindly reviews the resulting shadow text without receiving prior issue
explanations. By default these replacements remain read-only. With autofix
(or `pipeline.review_autofix: true`), folded `changes` are applied first; remaining
issues then reuse the same bounded Review Agent Loop against that updated text,
and confirmed issues reuse the same Review Fixer. The final complete segments are
written only to chapter `target`; no Review history fields are added to chapter
JSON, and the manifest and glossary are unchanged. Each run writes one user-facing
`result.json`, its model-usage
delta, an event stream, and internal round traces to
`state/<book>/targets/<target-language>/reviews/review-<timestamp>/`. The same usage delta is also added once
to the book's cumulative `usage.json`. Autofix keeps its full before/after chain,
issue decisions, failures, and idempotent write journal in `autofix/index.json`;
annotation and DOCX style offsets are refreshed after publishing. `report.json`
contains a compact Review and Autofix summary.
