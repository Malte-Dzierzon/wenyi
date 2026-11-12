# Wenyi Rust rewrite — consolidated plan

**Status:** 2026-11-12 · **Oracle pin:** `main@39068fcc` (user WIP excluded) ·
**Providers:** all 10 real kinds + `fake`

This is the single planning reference for the Rust rewrite. Repository cleanup
history is summarized in [`PLAN.md`](../PLAN.md); persistent engineering rules
are in [`WENYI_RUST_ENGINEERING.md`](../WENYI_RUST_ENGINEERING.md). Implemented
module boundaries are in [Architecture](architecture.md).

## 1. Goal and current status

Target: one native Rust frontend (`apps/desktop/native`, GPUI-Kit 0.8) and one
Rust engine (`crates/wenyi-core`). No Python runtime, no loopback HTTP, and no
web stack in the shipped app. Network access is limited to model providers and
explicitly enabled document/terminology services.

The UI still talks to the Python sidecar today. The Rust engine is being ported
slice by slice and must reach behavior parity before the sidecar is removed.

| Slice | Scope | Status |
|---|---|---|
| 0 | Golden corpus and baselines against the pinned oracle (`FakeClient`) | ✅ |
| 1 | Workspace, config parity, store skeleton, redaction, dependency guard | ✅ |
| 2 | Documents: text/Markdown, EPUB read/assemble, HTML, SRT, segmentation | ✅ |
| 3 | LLM routing/providers/usage/retry and bounded jobs | ✅ |
| 4 | One-book translation parity | **next** |
| 5 | Series, style, glossary curator | planned |
| 6 | Review, proofread, export parity | planned |
| 7 | Native in-process integration; remove Python reference | planned |

Current crate modules: `config`, `document`, `glossary`, `i18n`, `jobs`, `llm`,
`model`, `redact`, `store`. There is no `pipeline` module yet. The native crate
stays standalone until slice 7; joining the workspace earlier would rewrite its
lockfile and build graph for no benefit.

## 2. Decisions

D-prefix decisions are ratified; statuses were re-verified on 2026-11-12.

- **D1 — No web product.** `apps/web`, `apps/api`, `docker/`, `deploy/`, and the
  PG/Redis/arq stack are removed.
- **D2 — No Tauri shell.** The native GPUI app is the UI. Tray, updater, native
  dialogs, and other shell capabilities are reimplemented natively when needed.
- **D3 — Modular Rust monolith, single binary, SQLite.** One workspace database
  (`workspace.sqlite3`, WAL) is the baseline; per-project splitting needs scale
  evidence.
- **D4 — Configuration stays YAML** with the same schema as `config.yaml`.
  Defaults are byte-compared in tests.
- **D5 — Providers: trait + tiers + single-owner retry.** All 10 real kinds are
  ported (`deepseek`, `openai`, `openrouter`, `opencode-go`,
  `openai-compatible`, `orcarouter`, `ollama`, `vllm`, `llamacpp`, `gemini`)
  plus the scripted `fake` provider. Provider quirks stay in provider modules.
- **D6 — Preserve Desktop data.** A verified backup and a tested one-time
  importer read per-project `state.sqlite3` files. The catalog `local.sqlite3`
  is disposable scheduling state. CLI `state/` is out of scope. The old
  workspace stays untouched until validation passes.
- **D7 — PDF via HTTP-isolated bridge only.** No AGPL code in-tree; MinerU and
  BabelDOC remain external services.
- **D8 — No compatibility shims.** Python runs side by side only as a comparison
  oracle, then is deleted whole.
- **D9 — GPUI-Kit 0.8 is locked** during engine work; a toolkit upgrade is a
  separate change.
- **D10 — Prompts and i18n are embedded** (`include_str!`) and byte-compared.
  Translation-affecting changes follow the 50k-word evaluation rule.

## 3. Behavior oracle

Port behavior, not implementation shape:

```text
pipeline/orchestrator.py Orchestrator (routing/locks only)
  → translation / translation_batch / context / title_translation / precision
  → agents/{translator, analyzer, synopsis, polisher, reviewer, review_*}
  → glossary/store.py (upsert_term → inserted/unchanged/conflict; terms_in)
    + pipeline/curator.py (20 terms/pass, 5 evidence excerpts)
  → review_{workflow, chunks, rounds, checkpoint, autofix} + review/evidence.py
  → storage/{protocol, sqlite, artifacts} / runstore.py / srt/store.py
  → assemble/writer.py / ingest/tokens.py (cl100k_base)
  → i18n/data/tasks (32 files)
```

Invariants that must survive the port:

- `target is None` means pending; `""` means intentionally empty (MinerU
  exception). Never collapse them.
- Completed batches and chapters are skipped on resume.
- Review writes only a shadow translation. Autofix publishes through a
  pre-persisted recoverable index.
- Usage and events are append-once ledgers (`merge_usage_summaries`); retries and
  resumes must not double-bill.
- Segment, chapter, anchor, DOCX, and `babeldoc_id` identities stay stable.
- Short state locks are separate from long workflow locks.
- Secrets live only in environment variables or the OS credential store.
- SRT stays independent of the book pipeline.
- Parallel results merge in stable original order.
- The rolling-context floor is `max(40, rolling_context_segments)`.

Known weaknesses to fix in Rust:

- The style brief is built once per run; refresh only fills missing keys.
- The curator scans all chapters × segments per term.
- The project model is book-centric; series need first-class support.
- Translation inputs are loaded without a bounded working set.

Native UI baseline: 13 screens in `apps/desktop/native/src/views/`, all calling
`client.rs` over loopback REST and polling. Preserve screens and visual language;
replace `backend.rs`/`client.rs` with in-process `services.rs`/`events.rs` at
integration time.

## 4. Target architecture

```text
Cargo.toml                  # workspace, centralized versions, committed Cargo.lock
crates/wenyi-core/          # lib: store/llm/document/glossary/style/pipeline/jobs
apps/desktop/native/        # GPUI UI and app wiring only
```

Rules:

- `wenyi-core` never depends on GPUI, server frameworks, or UI types.
- The UI holds no business logic and no SQL.
- No storage-trait hierarchy until a second backend exists.
- No internal REST between UI and engine.

### Dependencies

Actual workspace dependencies (verified 2026-11-12):

- `tokio` 1 (`rt`, `rt-multi-thread`, `macros`, `sync`, `time`)
- `reqwest` 0.12 (`default-features = false`, `json`, `rustls-tls`) to match the
  native crate and avoid `openssl-sys` build failures
- `serde` / `serde_json`, `thiserror` 2
- `rusqlite` 0.41 (`bundled`, `backup`)
- `sha2` 0.11, `tiktoken-rs` 0.12
- `zip` 9 (`deflate-flate2-zlib-rs`), `quick-xml` 0.42
- `unicode-normalization` 0.1
- `serde_yaml` 0.9 — the 2026-11-10 spike parsed all fixtures identically to
  `serde-saphyr` and won on dependency weight

Planned when the corresponding slice lands: `tracing`, `keyring` (desktop
boundary), `aho-corasick` after matcher parity fixtures. Rejected until measured:
`rayon`, `sqlx`/ORMs, `Tantivy`/Elasticsearch, provider SDKs, actor frameworks.

### Planned schema

`workspace.sqlite3`, WAL, short transactions only:

- `projects` → `books` → `source_files` / `chapters` → `segments` +
  `translations` (`target NULL` = pending)
- `segment_revisions`
- `glossary_terms` (project-wide or per-book override; unique per
  project/languages/book/source) + `aliases` + `term_occurrences` (incremental)
- `style_profiles` + immutable `style_versions` + `proposals`
- `jobs` + `job_events`
- `usage_ledger` (`batch_key` idempotency)
- `review_runs` + `issues` + `publish_log` (autofix writes here first)
- `artifacts`, `schema_migrations`

Page by `(book, ordinal)`; chapters execute one at a time; the matcher snapshot is
shared per glossary version.

### Planned queues

- `translate` (~128), `export` (~32), `curate` (~1024 dirty-term references, low
  priority)
- Fixed workers (start 2/1/1); per-provider semaphores
- Same admission rules as today's `admit_job`: per-chapter lanes, curator
  exception
- Wave snapshots (`glossary_version`, `style_version`) merged in
  `(book, chapter, segment)` order
- Startup converts `running` → `interrupted`; cancellation happens at
  checkpoints
- UI keeps coarse ≤4 Hz progress events

## 5. Benchmarks and acceptance

Record B1–B12 against the pinned oracle before replacing behavior (`FakeClient`):

- cold start; project list without N+1 queries
- chapter page under 200 ms at 10k chapters
- segment commit under 100 ms
- progress events at ≤4 Hz
- bounded memory per stage
- indexed glossary find vs full scan
- incremental index updates
- queue saturation plus kill-9 exactly-once usage
- fake-HTTP provider suite
- atomic, byte-stable export
- binary size and build time

Corpora: a public-domain novel ≥50k words for human quality comparison, a
synthetic 10k-chapter project, CJK/multilingual matcher fixtures, and a
14k-page stress case. Never commit private books. Fake-model tests prove control
flow, not literary quality.

## 6. Migration sequence

0. Golden corpus and baselines (no Rust yet). ✅
1. Workspace, config parity, store skeleton. ✅
2. Documents, EPUB-first with round-trip goldens. ✅
3. LLM (all providers) and bounded jobs. ✅
4. One-book translation parity. **next**
5. Series, style, glossary curator.
6. Review, proofread, export parity.
7. Native in-process integration; remove the Python reference.

Each slice states its oracle-diff gate. No destructive migration or Python
removal before replacement and recovery tests pass.

## 7. Fork-specific behavior

These additions are already in the Python reference and must be carried into the
Rust engine:

- **Parallel chapter workers.** Three workflow lanes run chapter jobs
  concurrently. Per-chapter locks are authoritative for admission; book-wide
  jobs hold the run lock. Usage and timing merges happen under the short state
  lock so parallel workers cannot lose increments.
- **Glossary curator.** A separate `glossary_curator` job checks roughly 20 terms
  per pass against book evidence, holds `.curator.lock`, and never changes
  project status. Proposals become conflicts; the curator does not silently
  overwrite confirmed terms.
- **llama.cpp provider.** A local OpenAI-compatible gateway on
  `http://127.0.0.1:8093/v1`, with a `llamacpp` preset for local strong/cheap/fast
  models. Useful for offline or low-cost glossary work.
- **Style priority.** Explicit user style rules take precedence over generated
  observations; automatic refresh must not overwrite confirmed choices.

## 8. Cleanup summary

Removed in the 2026-11-10 cleanup: the web app and API, UI/shared-schema
packages, the CLI package, the Tauri shell, Docker/deploy assets, and the JS
toolchain. Retained: native GPUI app, Python sidecar, Python engine reference,
sidecar HTTP layer, golden fixtures, and the Rust workspace. Details and evidence
remain in Git history; the short marker is [`PLAN.md`](../PLAN.md).

## 9. Open questions and deferred work

Open questions:

- **Q2:** evaluation corpus — 1–2 public-domain novels ≥50k words with recorded
  checksums.
- **Q3:** PDF extraction crate with a MIT-compatible license.
- **Q4:** updater signing and distribution.
- **Q5:** keyring round-trip on each OS.
- **Q6:** DOCX fidelity caveats.
- Series UX (multi-volume split/group), style auto-accept thresholds, and
  chapter-worker concurrency defaults (benchmark 1/2/N).
- Whether a shipped CLI returns (owner decision).

Deferred: multi-user/cloud sync, plugins, embedded inference, automatic rewrite
of completed chapters, visual redesign, toolkit upgrade, required FTS5, web
lookup, and a native PDF engine.
