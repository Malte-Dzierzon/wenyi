# 09 — Document Processing

Per-format current implementation (verified against `packages/core/wenyi_core/ingest/`, `assemble/`, `markup/`, `pdf_bridge/`, `srt/`, `postprocess/`) and the target Rust design. Engine stages are doc 07; provider layer is doc 08.

## 1. Core data model

`ingest/models.py`: `Segment` (smallest alignable/backfillable unit — paragraph or heading; `index`, `source`, `kind` text/heading, `target`, `target_before_polish`, `anchor`, `resource_href`, `cont`, `meta`), `Chapter` (`index`, `title`, `segments`, `href`, `template`, `meta`; `text_segments` = nonempty-source segments), `Document` (`title`, `source_lang`, `target_lang`, `fmt`, `chapters`, `meta`). Batches require an equal-length translation array so alignment errors surface instead of silently dropping paragraphs (`ingest/models.py` header; enforced `agents/translator.py:Translator._call_batch()`).

Segmentation: `ingest/segmenter.py` — `load_document()` (extension dispatch + optional `split_long_segments()`), `batch_segments()` (shared token budget, tiktoken `cl100k_base` via `ingest/tokens.py:count_tokens()`), `split_long_segments()` (sentence-split at `_SENT_SPLIT`, mark `cont=True`, writer merges back). BabelDOC paragraphs are never token-split (`load_document()` skips splitting when `doc.meta.babeldoc`).

## 2. Per-format current implementation

Dispatch: `ingest/segmenter.py:load_document()` (`.epub / .md/.markdown/.txt/.text / .fb2 / .html/.htm/.xhtml / .pdf / .docx`; SRT handled separately via `ingest/srt_reader.py:parse_srt()` + `srt/translate.py:translate_srt()`). Output formats (`assemble/writer_common.py:_OUT_EXT`): `epub / txt / html / markdown / pdf / docx`; default is `pdf` for BabelDOC state else `epub` (`default_output_format()`); entry `assemble/writer.py:assemble()`; bilingual naming `bilingual_out_path()`.

| Format | Reader | Writer | Notes |
|--------|--------|--------|-------|
| EPUB | `ingest/epub_reader.py:read_epub()` (+ `peek_epub_title()`; package `ingest/epub_package.py`, TOC `ingest/epub_toc.py`, layout `ingest/epub_layout.py`, chapters `ingest/epub_chapters.py`) | `assemble/epub_writer.py` (`_assemble_epub()`, `_build_epub_from_chapters()`, `_build_epub_from_html_templates()`) + navigation/presentation/resources (`assemble/epub_navigation.py`, `epub_presentation.py`, `epub_resources.py`) | ZIP via stdlib `zipfile`; markup via bs4/lxml + ebooklib. Physical spine resources → logical chapters from top-level TOC anchors; `fragment_anchor_map()` (`markup/anchors.py:13`); backfill by placeholder/`anchor` IDs; images/resources preserved |
| FB2 | `ingest/fb2_reader.py:read_fb2()` (+ `peek_fb2_title()`, `read_fb2_binaries()`) | EPUB writer (no native FB2 writer) | XML sections → chapters; binaries extracted for resource parity |
| TXT/MD | `ingest/text_reader.py:read_text()` | `assemble/text_writer.py` (`_assemble_plain_text()`, `_assemble_text()`, `_assemble_markdown()`) | Heading detection `_is_chapter_heading()`; titles kept as heading segments |
| HTML | `ingest/html_reader.py:read_html()` (`_extract_chapter()`) | `assemble/html_writer.py:assemble` (`_assemble_html()`) + `html_bilingual.py`, `html_inline.py`, `html_renderer.py`, `html_resources.py` | Source templates reused when available, else rebuilt chapters |
| DOCX | `ingest/docx_reader.py:read_docx()` | `assemble/docx_writer.py:assemble` (`_assemble_docx()`) + blocks/numbering/styles (`assemble/docx_blocks.py`, `docx_numbering.py`, `docx_styles.py`) | python-docx; body-block walk (`_iter_body_blocks()`), outline levels, list meta, run styles, shading; style policy `document_styles/docx.py`, applied via `pipeline/docx_styles.py:DocxStyleService` |
| PDF (MinerU, default) | `ingest/pdf_reader.py:read_pdf()` → `ingest/pdf_to_html.py:convert_pdf_to_html()` (`MinerUApi`: `submit_batch()`, `_poll_batch()`, `_download_zip()`; `_split_pdf()`, `_html_from_zip()`, `_assemble_html()`) | `assemble/pdf_writer.py:assemble` (`_assemble_pdf_weasyprint()` default, `_assemble_pdf_fpdf2()` fallback, `_assemble_pdf_babeldoc()` for translation backfill) | Cloud conversion; HTML cached in run state (`pdf_cache_html_path()`); pypdf for page counts/TOC (`ingest/pdf_babeldoc.py:toc_chapter_starts()`); `""` empty-target allowance for MinerU PDFs (`pipeline/translation.py:_is_mineru_pdf()`) |
| PDF (BabelDOC bridge) | `ingest/pdf_babeldoc.py:read_pdf_babeldoc()` (+ `_segment_from_para()`, `_chapters_from_paragraphs()`, `translations_from_store()`) via `pdf_bridge/client.py:BabeldocBridgeClient` (`health()`, `extract()`, `fillback()`) | `_assemble_pdf_babeldoc()` | HTTP-only bridge to the standalone AGPL BabelDOC service (license isolation); image-only detection (`_is_image_only_pdf()`); backend selected by `pdf_backend="babeldoc"` |
| SRT | `ingest/srt_reader.py:parse_srt()` (`SrtCue`, `_decode_srt_bytes()`) | `assemble/srt_writer.py` (`write_srt_outputs()`, `default_srt_out_paths()`) | Sliding-window translation `srt/translate.py:translate_srt()`; store `srt/store.py:SrtRunStore`; policy `srt/policy.py:export_plan()` |

Structure preservation: `markup/segments.py:annotate_epub_resource()` (block → segment annotation, `INLINE_META_KEY` contract in `markup/contracts.py`), `markup/anchors.py` (fragment maps, `resolve_epub_href()`), `markup/annotations.py` (footnote/endnote relation detection, range markers, note scopes), `markup/ruby.py` (ruby stripping at batch boundary: `strip_ruby_markers()` in `TranslationBatchExecutor.execute()`), annotation alignment after each batch (`AnnotationService.align_annotations_after_batch()`, agent `agents/annotation_aligner.py:AnnotationAligner`).

Export integrity: `assemble/policy.py` (`export_options()`, `export_plan()`), punctuation normalization in export copies only (`postprocess/`: `export_text.py`, `punct.py`), EPUB round-trip via template rebuild (`_build_epub_from_html_templates()`), bilingual output (`html_bilingual.py`, `bilingual_out_path()`, `preserve_source_style`), about page (`assemble/about.py`, `document_styles/docx.py`), report (`assemble/report.py:build_report()`), export snapshots (`ExportSnapshotStore`, `pipeline/runstore.py:324`; `ExportViewStore`, `assemble/export_view.py`).

## 3. Target Rust design: EPUB-first crate

Crate layout (under the monolith layout of doc 05):

- `wenyi-doc` — `Segment`/`Chapter`/`Document` types (port of `ingest/models.py`, `cont` merge semantics included), tokenizer-budget helpers (ported budget table; tiktoken replaced by a local BPE/count table calibrated against `count_tokens()` fixtures).
- `wenyi-epub` — `parse → segment → backfill → verify` sub-stages: package reader (ZIP + OPF/manifest/TOC), resource annotator (port of `markup/segments.py` + `anchors.py` + `annotations.py` semantics), chapter builder (port of `epub_layout.py`/`epub_chapters.py` logical-chapter rules), template backfill writer, round-trip verifier.
- `wenyi-formats` — TXT/MD/HTML/SRT/FB2/DOCX readers + writers behind the same `Document` interface; PDF-last (see below).
- Shared: annotation/ruby/contracts module used by both EPUB and HTML paths; style-policy module (port of `document_styles/docx.py` purity: no model calls in styling).

Deterministic round-trip tests: source → parse → (identity translation: target = source) → assemble → re-parse must reproduce segment count, order, anchors, TOC entries, and resource set. Golden EPUB corpus (small crafted books covering footnotes, ruby, images, nested TOC, multi-resource chapters) checked in as fixtures; any backfill mismatch fails the build.

## 4. Format priority order

1. **EPUB** — primary format; full round-trip fidelity (anchors, TOC, resources, annotations).
2. **TXT/MD/HTML** — heading detection + template reuse semantics ported; straightforward.
3. **SRT** — cue parsing + sliding-window batching + timing-preserving writer.
4. **FB2** — reader parity (sections, binaries); export via EPUB writer as today.
5. **DOCX** — block walk, outline/list/run-style fidelity, style policy port.
6. **PDF last** — MinerU HTTP conversion and BabelDOC HTTP bridge kept as external-service clients with identical protocol behavior; local PDF rendering via print-HTML. No native PDF layout engine in v1.

### Acceptance criteria

- [ ] `Segment`/`Chapter`/`Document` + `cont`-merge + batch-budget grouping match Python behavior on the shared fixture corpus (oracle tests vs exported Python JSON).
- [ ] EPUB round-trip suite green: identity-translation output re-parses to identical segment/anchor/TOC/resource sets; footnote/ruby/image fixtures included.
- [ ] Each format reader has a golden-file test (source → `Document` JSON snapshot) traced to the reader/writer paths in §2.
- [ ] PDF paths covered by bridge-protocol contract tests (recorded HTTP fixtures), not live services.
- [ ] Bilingual + punctuation-normalize export semantics preserved (export copies only, never stored targets).

## Sources

`packages/core/wenyi_core/ingest/models.py`, `segmenter.py`, `tokens.py`, `source_hash.py`, `epub_reader.py`, `epub_package.py`, `epub_toc.py`, `epub_layout.py`, `epub_chapters.py`, `fb2_reader.py`, `text_reader.py`, `html_reader.py`, `docx_reader.py`, `pdf_reader.py`, `pdf_to_html.py`, `pdf_babeldoc.py`, `srt_reader.py`, `markup/anchors.py`, `annotations.py`, `segments.py`, `contracts.py`, `ruby.py`, `assemble/writer.py`, `writer_common.py`, `epub_writer.py`, `epub_navigation.py`, `epub_presentation.py`, `epub_resources.py`, `docx_writer.py`, `docx_blocks.py`, `docx_numbering.py`, `docx_styles.py`, `text_writer.py`, `html_writer.py`, `html_bilingual.py`, `html_inline.py`, `html_renderer.py`, `html_resources.py`, `pdf_writer.py`, `srt_writer.py`, `policy.py`, `report.py`, `about.py`, `export_view.py`, `pipeline/annotations.py`, `pipeline/docx_styles.py`, `document_styles/docx.py`, `pdf_bridge/client.py`, `srt/translate.py`, `srt/store.py`, `srt/policy.py`, `postprocess/export_text.py`, `postprocess/punct.py`, `agents/annotation_aligner.py`, `agents/translator.py`.

## Open questions

See doc 16 (`16-decisions-and-open-questions.md`) for the consolidated list.
