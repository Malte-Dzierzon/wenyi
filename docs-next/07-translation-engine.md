# 07 — Translation Engine

Current pipeline behavior (verified against `packages/core/wenyi_core/`) and the target Rust engine design. Agent-loop details are in doc 06; formats are in doc 09 and storage schema in doc 12; GUI and OS integration are out of scope here.

## 1. Current pipeline, end to end

Entry facade: `pipeline/orchestrator.py` (`Orchestrator`). It owns step routing and lock scopes only; parsing, model calls, state I/O, glossary, alignment, and exports belong to domain services. Layering is `CLI / Web / Desktop → Orchestrator → Runtime / Preparation / Translation / Annotation / Review / ReviewAutofix / Finalization → agents / ingest / glossary / assemble / Storage` (`pipeline/orchestrator.py` header; `AGENTS.md` architecture boundary).

`Orchestrator.ALL_STEPS = ("translate", "review", "report", "assemble")` (`pipeline/orchestrator.py:33`). `run_all()` runs translate → review (if `pipeline.review`) → report → assemble via `run_steps()`/`_finish_steps_locked()` (`pipeline/orchestrator.py:397`).

| # | Stage | Owner | Entry symbols |
|---|-------|-------|---------------|
| 0 | Preparation (parse, locate/init state, detect language, prescan, analysis, synopsis) | `pipeline/preparation.py` (`PreparationService`) | `prepare()`, `prepare_for_translation()`, `ensure_understanding()` |
| 1 | Body translation (batches, polish, glossary extraction, annotation alignment) + chapter/title translation | `pipeline/translation.py` (`TranslationService`); batches `pipeline/translation_batch.py` (`TranslationBatchExecutor`); titles `pipeline/title_translation.py` (`TitleTranslationService`) | `TranslationService.run()`, `translate_chapter()`, `TranslationBatchExecutor.execute()` |
| 2 | Glossary (SQLite store, extraction, conflict log, injection into prompts) | `glossary/store.py` (`GlossaryStore`), `glossary/extractor.py` (`GlossaryExtractor`), `glossary/resolver.py` (`resolve()`), prompt rendering `agents/prompts.py` (`render_glossary()`) | `extract_and_store()`, `upsert_term()`, `resolve()` |
| 3 | Polish (continuation or standalone) | `agents/polisher.py` (`Polisher.polish()`, `polish_continue()`) | called from `TranslationBatchExecutor.execute()` |
| 4 | Titles (chapter + TOC, after body) | `pipeline/title_translation.py` (`plan_titles()`, `title_batches()`, `TitleTranslationService.run()`), agent `agents/title_translator.py` | runs when `not store.pending_chapters()` (`pipeline/translation.py`) |
| 5 | Review (read-only shadow review, parallel blocks, evidence loop, arbitration, fix loop) | `pipeline/review_workflow.py` (`ReviewService.run_session()`), chunks `pipeline/review_chunks.py` (`ReviewChunkService`), rounds `pipeline/review_rounds.py` (`ReviewRoundService`), results `pipeline/review_results.py`, checkpoint `pipeline/review_checkpoint.py` | `run_session()` |
| 6 | Autofix publish (recoverable index → formal chapters) | `pipeline/review_autofix.py` (`ReviewAutofixService`), plan `pipeline/autofix_plan.py` (`prepare_identity()`, `save_plan()`), publish `pipeline/autofix_publish.py` (`AutofixPublisher.apply()`), verify `pipeline/autofix_verification.py`, candidates `pipeline/autofix_candidates.py` | `ReviewAutofixService.run()`, `resume_pending()` |
| 7 | Report | `pipeline/finalization.py` (`ReportService.build_and_save()` → `assemble/report.py:build_report()`) | `build_and_save()` |
| 8 | Assemble (mono/bilingual export, live or snapshot) | `pipeline/finalization.py` (`AssemblyService.assemble_live()`, `assemble_snapshot()`) → `assemble/writer.py:assemble()` | `assemble_live()`, `assemble_snapshot()` |

Input parsing itself (reader selection) is model-free and lives in `pipeline/input_preparation.py` (`load_parsed_document()`, `locate_input_storage()`, `parse_document()`); language detection is the model-backed part of `PreparationService`.

### State files and locks

Per-run directory: `state/<slug>/targets/<lang>/` with `RunStore` (`pipeline/runstore.py:48`; `translation_run_dir()` at `pipeline/runstore.py:42`, `slugify()` at `pipeline/runstore.py:36`). Artifacts:

- `manifest.json` — staged by `RunStore.stage_document()` (`pipeline/runstore.py:248`), committed atomically **last** (`pipeline/runstore.py:351` `save_manifest()`); chapter rows carry `status` pending/done (`STATUS_PENDING`/`STATUS_DONE`, `pipeline/runstore.py:32`).
- `chapters/<i>.json` — `save_chapter()` / `save_chapter_with_status()` (`pipeline/runstore.py:376`); resume key.
- `context.json` — `save_context()`/`load_context()` (`pipeline/runstore.py:397`).
- `analysis.json` — `save_analysis()`/`load_analysis()` (`pipeline/runstore.py:417`).
- `glossary.db` — SQLite at `glossary_path()` (`pipeline/runstore.py:136`).
- `usage.json` (+ pending deltas) — `save_usage()`, `prepare_usage_commit()`, `recover_usage()` (`pipeline/runstore.py:429`); append-once ledger semantics.
- `events.jsonl` — `log_event()` path via `storage/artifacts.py`; assembly/finalization also in scope.
- `reviews/<review-id>/` — `ReviewRunStore` (`review/run_store.py`); `result.json`, `autofix/index.json`, per-round metadata, agent traces (`pipeline/review_checkpoint.py:ReviewTraceStore`).
- `report.json` — `save_report()` (`pipeline/runstore.py:425`).
- `.initializing` marker — `begin_initialization()`/`finish_initialization()` (`pipeline/runstore.py:190`).

Locks (`pipeline/runstore.py:64`, primitive `storage/locks.py:exclusive_file_lock()`):

- `lock()` — book run lock; every mutating stage runs under it.
- `state_lock()` — short lock for chapter reads/writes inside translation.
- `event_lock()` — event appends outside worker threads (`pipeline/review_workflow.py` restores order, writes sorted recovery events outside threads).
- `assemble_lock()` — serializes output writers; standalone `assemble_snapshot()` captures an immutable snapshot under assembly/state locks, releases the state lock before rendering, and validates source hashes before and after (`pipeline/finalization.py:AssemblyService`).

### Resume rules (verified)

- `target is None` = pending; `""` (empty string) = completed — **except** MinerU PDFs, where empty targets are an allowed completion. Detection: `_is_mineru_pdf()` (`pipeline/translation.py:34`); `allow_empty_translations` threads through `BatchPlan` (`pipeline/translation_batch.py`) and `Translator.translate_batch()`. `progress_counts()` counts a batch done only when every target is not `None`, and notes blank `""` counts (`pipeline/translation.py:progress_counts()`).
- `resume_batches()` (`pipeline/translation_batch.py:117`) re-splits token batches at completed/pending boundaries so reruns translate only missing paragraphs and never overwrite confirmed content.
- Completed batches are reused positionally: targets kept, rolling context rebuilt, annotations re-aligned (`align_annotations_after_batch()`), then skipped (`pipeline/translation.py:translate_chapter()`).
- Glossary checkpoints: `store.completed_batch_glossary_keys(ci)` / `batch_glossary_key()`; saved translations lacking extraction still extract and mark the snapshot stale, without retranslating (`pipeline/translation.py:translate_chapter()`).
- Review resume: `ReviewCheckpoint.restore()` restores `target_overrides`, `seen_overlays`, `patch_records`, `fix_failures` within one review dir; completed reviews are reused only when content digest, config snapshot, and glossary fingerprint all match (`ReviewService._review_skip_eligible()`, `pipeline/review_workflow.py`). Autofix resume: `resume_pending()` finishes an `applying` index or completes an unrecorded finished index before any new review (`pipeline/review_autofix.py`).
- Source identity: `source_sha256()` (`ingest/source_hash.py`, `pipeline/runstore.py`); `ensure_source_identity()` rejects mismatched inputs; PDF state follows the filename and preserves identity/events across failed conversions (`pipeline/preparation.py:prepare()`).

## 2. Context strategy (current)

Three layers, deliberately split by mutation rate:

1. **Fixed global prefix** — whole-book synopsis (`Synopsizer.book_synopsis()`) + per-chapter source digests (`chapter.meta["source_digest"]`, `digest_chapter()`; budget: first 8000 chars per chapter) + style brief (`Analyzer`, `pipeline/preparation.py:_prepare_locked()`). Grouped map-reduce merging bounds prompt length (`_REDUCE_BUDGET = 12000` chars, `agents/synopsis.py`). Stable across batches → cache reuse. Reused when complete (`_synopsis_complete()`, `_digest_complete()`, `pipeline/preparation.py:52`).
2. **Chapter digest** — current chapter's digest passed per batch (`chapter_digest` in `BatchPlan`, `pipeline/translation_batch.py:17`).
3. **Rolling suffix** — `RollingContext` (`pipeline/context.py:13`): last N translated targets (`max_recent_keep = 40` default floor; `max(40, pipeline.rolling_context_segments)`), rendered per batch via `render(n_recent)`, persisted to `context.json` after each chapter (`TranslationService.run()`).

Plus per-batch: glossary terms in stable insertion order (`term_snapshot`, refreshed lazily when stale), style string, annotation contexts slice, and the immediate `next_source` neighbor (read without changing batches). `BatchPlan.capture()` detaches mutable term/annotation metadata per batch (`pipeline/translation_batch.py:73`).

Sampling for prescan: `sample_text()` in `PreparationService`; language detection uses unlabeled samples so labels cannot contaminate detection.

## 3. Multi-stage configurability (current)

`config.py:PipelineConfig` (`packages/core/wenyi_core/config.py:103`): `translation_mode` (`standard | best_of_three`, requires `polish=true`, validated at `config.py:114`), `polish` (default on), `review`, `review_autofix`, `book_understanding`, `rolling_context_segments`, review knobs (`review_concurrency=4`, `review_output_retries=2`, `review_agent_loop`, `review_agent_max_evidence_rounds=2`, `review_conflict_arbitration`, `review_fix_loop`, `review_fix_max_rounds=2`, `review_clean_confirmations=2`).

- `best_of_three`: `TranslationService.translate_chapter()` routes through `PrecisionBatchExecutor` (`pipeline/precision.py:27`) with usage checkpointing per phase; metadata/persistence helpers `prepared_publication()`, `mark_published()`, `recover_publications()` (`pipeline/precision.py:49`).
- Operation routing per stage is flag-driven: `configured_operations()` / `workflow_operations()` (`llm/operations.py:171`) map pipeline flags to the operation registry (e.g. `polish.body` requires `polish`, `autofix.*` require `review_autofix`).

## 4. Glossary (current)

- Store: `GlossaryStore` (`glossary/store.py:218`), SQLite (`initialize_schema()` at `glossary/store.py:240`); terms `GlossaryTerm` (`glossary/store.py:34`); `upsert_term()` + `_log_conflict()` (`glossary/store.py:309`); `resolve_term()` / `mark_conflicts_resolved()` / `open_conflicts()` / `stats()`; matching via `source_matches_text()` (`glossary/store.py:134`), `term_match_sources()`, `GlossaryOccurrenceMatcher.recurring_terms()`.
- Extraction: `GlossaryExtractor.extract()` / `extract_and_store()` (`glossary/extractor.py:87`) with `TranslatedSegmentEvidence` history and `_align_with_first_occurrences()`; seeded from style analysis (`Analyzer.seed_glossary()`, called in `PreparationService._prepare_locked()`).
- Resolution: `glossary/resolver.py:resolve()` (single-function module).
- Injection: `agents/prompts.py:render_glossary()` (`agents/prompts.py:10`), consumed by `Translator` (`agents/translator.py:118`) and `Synopsizer`/others.
- Review-side binding: review glossary fingerprint over the full ordered prompt (`ReviewService._review_glossary_fingerprint()`); normalization consults `evidence.canonical_term()` (`review/conflicts.py:normalize_review_issues()`).

## 5. Review / QA (current)

Read-only by construction: each review uses an independent directory, updates only its shadow overlay; formal chapters, manifest, and glossary stay read-only (`pipeline/review_workflow.py` header). Flow:

1. **Scan** — `ReviewChunkService.review_chapter()` (`pipeline/review_chunks.py:30`): parallel blocks over a read-only snapshot (`review_concurrency`), output retries (`review_output_retries`), adaptive chunking (`review_adaptive()`, `pack_contiguous()`, `try_cached_subchunks()`), sorted recovery events outside threads.
2. **Evidence loop** — `ReviewAgentLoop.review_chunk()` (`agents/review_loop.py:65`) with bounded rounds (`review_agent_max_evidence_rounds`); index `BookEvidenceIndex` (`review/evidence.py:29`) — cross-chapter context + term occurrences through bounded queries, shadow overrides visible only in-index.
3. **Arbitration** — `ReviewConflictArbiter.arbitrate()` (`agents/review_arbiter.py:38`); conflict groups `review/conflicts.py:build_conflict_groups()`; application `apply_review_arbitrations()`.
4. **Fix loop** — `ReviewSessionState.prepare_fix()` / `after_fix()` (`review/session.py:265`) propose shadow replacements; `ReviewFixer` (`agents/review_fixer.py:123`) with blind re-review; digests `review_overlay_digest()` / `content_digest()` (`review/session.py:26`); rounds `ReviewRoundService.review_round()` / `propose_review_patches()` (`pipeline/review_rounds.py:43`).
5. **Publish** — only via `ReviewAutofixService` on an immutable snapshot: candidates (`pipeline/autofix_candidates.py`), identity/hash plan (`prepare_identity()`, `save_plan()`), `autofix/index.json` persisted **before** formal mutation, idempotent `AutofixPublisher.apply()` with optimistic `before_hash` checks (`pipeline/autofix_publish.py`), verification (`pipeline/autofix_verification.py`).

Deterministic vs LLM split: normalization (`normalize_review_issues()`), conflict grouping, net-change computation (`net_changes()`), issue projection (`project_issues()`), and hash/identity checks are deterministic (`review/conflicts.py`, `pipeline/review_results.py`, `review/models.py`); scanning, evidence verification, arbitration, and fix proposals are LLM operations (`review.scan`, `review.verify`, `review.arbitrate`, `review.fix` tiers in `llm/operations.py`).

## 6. Target engine design (Rust)

Modular monolith, single binary; pipeline as explicit stage DAG over a SQLite-backed run store (schema in doc 12).

- **Stage pipeline**: `prepare → translate(body+glossary+polish) → titles → review → autofix → report → assemble`. Each stage is a pure function over `(RunStore, StageInput) -> StageOutput`; stages declare inputs/outputs; the orchestrator owns ordering, lock scopes, and progress events only (mirror `Orchestrator`'s no-domain-I/O rule). `translation_mode` (`standard | best_of_three`) and `polish`/`review`/`review_autofix`/`review_*` flags port 1:1 as stage selectors; flag-to-operation mapping becomes a compile-time table (replacing `workflow_operations()`).
- **Checkpoint format**: per-chapter JSON rows equivalent to `chapters/<i>.json` become SQLite rows (`segments` table, `target NULL` = pending, `""` = complete; MinerU allowance as an explicit per-run bool column, not a filename sniff). Batch checkpoints persist `(chapter, batch_start, len)` + glossary checkpoint keys so resume re-derives `resume_batches()` grouping deterministically. Manifest commit stays atomic-last. Review checkpoints persist `target_overrides`, `seen_overlays`, patch records; reuse requires content-hash + config-snapshot + glossary-fingerprint equality.
- **Context budget for 10k pages**: never full-book context. Fixed prefix (synopsis ≤ configured chars + current-chapter digest + style brief + glossary terms filtered to terms occurring in/near the batch) + rolling suffix (last N targets, N configurable, default 40 segments) + one `next_source` neighbor. Target caps: per-batch source ≤ `max_tokens_per_batch` (tiktoken-equivalent budget via the ported tokenizer table), synopsis merge groups ≤ 12k chars. Long-book synopsis uses the same grouped map-reduce. Chapter digests persist per chapter; missing digests rebuild lazily without touching formal targets.
- **Glossary**: port `GlossaryStore` schema + matcher semantics exactly (including conflict log and resolution flags); extraction evidence = translated-history index rebuilt from the `segments` table; injection renderer byte-equivalent to `render_glossary()` for prompt-parity tests.
- **Review/QA**: port shadow-overlay discipline (formal rows read-only during review; overlay table + content digests), deterministic normalization/grouping/net-change code verbatim in behavior, and the publish path with pre-persisted index + optimistic `before_hash` checks. Parallelism via scoped threads with in-order result merge (replacing `ThreadPoolExecutor` + outside-thread event writes).

### Acceptance criteria

- [ ] Stage table (§1) ports with identical ordering, skipping, and resume semantics; `target NULL` pending / `""` complete (+ MinerU allowance flag) verified by round-trip tests against fixtures exported from the Python runner.
- [ ] `RollingContext` render/add/persist behavior has a byte-level unit test; context-budget caps hold on a synthetic 10k-page book (batch prompt size bounded, no full-book materialization).
- [ ] `resume_batches()` grouping and completed-batch reuse ported exactly (property test vs Python oracle on randomized completion patterns).
- [ ] Glossary matcher + conflict lifecycle matches Python behavior on the shared golden corpus.
- [ ] Shadow-review + indexed autofix publish passes the interrupted-publish recovery test (kill mid-apply → resume completes idempotently, no formal-row corruption).

## Sources

`packages/core/wenyi_core/pipeline/orchestrator.py`, `preparation.py`, `translation.py`, `translation_batch.py`, `context.py`, `title_translation.py`, `review_workflow.py`, `review_checkpoint.py`, `review_chunks.py`, `review_rounds.py`, `review_results.py`, `review_autofix.py`, `autofix_plan.py`, `autofix_publish.py`, `autofix_candidates.py`, `autofix_verification.py`, `finalization.py`, `input_preparation.py`, `language_policies.py`, `precision.py`, `precision_records.py`, `runstore.py`, `agents/synopsis.py`, `agents/translator.py`, `agents/polisher.py`, `agents/title_translator.py`, `agents/annotation_aligner.py`, `agents/review_loop.py`, `agents/review_arbiter.py`, `agents/review_fixer.py`, `agents/prompts.py`, `glossary/store.py`, `extractor.py`, `resolver.py`, `review/evidence.py`, `session.py`, `conflicts.py`, `models.py`, `run_store.py`, `autofix_models.py`, `storage/locks.py`, `ingest/models.py`, `segmenter.py`, `source_hash.py`, `assemble/writer.py`, `report.py`, `policy.py`, `config.py`, `i18n/policy/models.py`.

## Open questions

See doc 16 (`16-decisions-and-open-questions.md`) for the consolidated list.
