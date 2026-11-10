# Module responsibilities

[简体中文](zh/architecture.md) · [Pipeline](pipeline.md) · [Desktop](desktop.md)

This is a map of the implemented module responsibilities and boundaries.
Core paths below are relative to `packages/core/wenyi_core/`.

For the target architecture, see the [Rust rewrite plan](RUST-REWRITE.md).

| Area | Owner |
|---|---|
| Entry points | The native UI lives in `apps/desktop/native/`; the local Desktop sidecar entry point lives in `apps/desktop/backend/wenyi_desktop/`. |
| HTTP applications | `packages/backend/wenyi_backend/` owns shared routes, schemas, application services and platform ports. Each app and worker receives its own backend context; platform adapters supply persistence, task scheduling, credentials and telemetry. |
| User interface | `apps/desktop/native/` owns the GPUI screens, widgets and app wiring. It calls the engine over the loopback sidecar contract until the in-process Rust engine lands. |
| Pipeline routing | `pipeline/orchestrator.py` assembles services and owns routing and lock scopes; domain work stays in the services. |
| Source preparation | `pipeline/input_preparation.py` owns model-free parsing, source/config identity and source snapshots; sidecar previews and `PreparationService` share it. `packages/backend/wenyi_backend/source_view.py` projects uninitialized source chapters read-only; model-assisted initialization remains in `pipeline/preparation.py`. |
| Translation | `pipeline/translation.py` coordinates translation; `translation_batch.py` returns explicit batch results; `title_translation.py` handles titles. |
| Whole-book review | `pipeline/review_workflow.py` coordinates sessions; `review_checkpoint.py`, `review_rounds.py`, `review_chunks.py`, and `review_results.py` separate recovery, decisions, execution, and result handling. |
| Autofix | `pipeline/review_autofix.py` coordinates the separate publication service; `autofix_candidates.py`, `autofix_plan.py`, and `autofix_publish.py` separate candidates, the recoverable index, and writes to formal translations. |
| Review agents | `agents/review_*.py` owns model interactions; `review/` provides shared evidence, types, and run artifacts without depending on pipeline orchestration. |
| EPUB / HTML | `markup/` owns shared deterministic anchors, annotations, ruby and segment handling; readers and writers remain in `ingest/` and `assemble/`. |
| DOCX | `document_styles/docx.py` owns pure style policy; DOCX readers and writers own parsing and document emission. |
| Persistence | Domain services use `storage/protocol.py`. File state uses `storage/file.py`; Desktop injects `storage/sqlite.py` into its own workspace. |

Keep Core independent of HTTP frameworks. Agents must not depend on the pipeline
or concrete state stores. Shared markup and style policy must not depend on LLMs or a
particular writer. SRT remains an independent lightweight path; BabelDOC remains an
external HTTP service.

Desktop owns a SQLite catalog, per-project SQLite stores, and a local runner.
The shared backend does not import platform packages. The sidecar is built in a
separate environment without PostgreSQL, Redis, or Arq libraries.

Initialization commits the manifest last; publication uses a recoverable index before
changing formal targets. Stable segment identities, consistent export snapshots, and
once-only usage accounting remain contracts across storage backends. See the
[repository guide](../AGENTS.md) for the full constraints and required verification.

`i18n/policy/` provides the pure typed language-operation registry and deterministic resolver. Profiles keep source and target roles separate; plans freeze the consumed prompt resources and selected export capabilities. Agents consume prompt plans, export adapters execute text and writer operations, and pipeline services persist phase identities through Storage. Policy bindings and options are defined in source, without YAML switches. Semantic checkpoints and export snapshots have separate identities, so layout changes do not invalidate paid translation. See [built-in language policies](configuration.md#built-in-language-policies) for supported behavior.

## Parallel chapter workers (custom fork)

Locks, from coarse to fine:

| Lock | Scope | Held by |
|---|---|---|
| `.run.lock` | Whole project | Book-wide jobs (`translation`, `prepare`, `review`), brief book ops (terminal status, reports) |
| `.chapter-<ci>.lock` | One chapter | `chapter_translation` workers and their admission (`project_write(chapter=…)`) |
| `state_lock()` | One short DB transaction | Every read-modify-write (`save_chapter`, `flush_usage`, `record_timing`, …); reentrant via savepoints |

Rules:

- The active-jobs table is the authoritative conflict oracle (`admit_job`,
  checked atomically in the admission transaction). File locks only guard
  against worker races, never decide admission.
- Chapter jobs on different chapters run concurrently (3 workflow lanes in
  `LocalRuntime`). Same chapter twice, or a book job while any chapter job
  is active, is rejected with 409.
- `project_write(chapter=N)` must NOT probe `.run.lock`: brief book
  operations hold it and would wrongly reject chapter starts.
- Usage/timing merges run inside `state_lock` so parallel workers cannot
  lose each other's increments (regression test:
  `test_parallel_flush_usage_keeps_all_deltas`).
- Terminal project status is set only when no sibling job is still
  `queued`/`running` (`_record_terminal_status`).
