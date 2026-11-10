# Wenyi Rust rewrite — consolidated plan

**Date:** 10 November 2026 · **Oracle pin:** `main@39068fcc` (user WIP excluded) · **Providers:** all 10 real kinds + fake

This document consolidates the retired `docs-next/` plan (git history keeps the originals) with
corrections from the 2026-11-10 architecture audit. It is the single planning reference for the
Rust rewrite. `PLAN.md` covers the repository cleanup that precedes implementation.

## 1. Vision and decisions

Target: one native Rust frontend (`apps/desktop/native`, GPUI-Kit 0.8) + one Rust engine
(`crates/wenyi-core`). No Python runtime, no loopback HTTP, no web stack in the shipped app.
Network access limited to model providers and explicitly enabled document/terminology lookups.

Decisions (D-prefix = ratified; statuses re-verified 2026-11-10):

- **D1 — No web product.** `apps/web`, `apps/api`, `docker/`, `deploy/`, PG/Redis/arq removed.
- **D2 — Tauri shell removed.** Native GPUI app is the UI; Tauri patterns (tray, updater, native
  dialogs) reimplemented as native equivalents when needed.
- **D3 — Modular Rust monolith, single binary, SQLite.** One workspace DB (`workspace.sqlite3`,
  WAL) is the benchmark baseline; per-project split only if scale tests justify it.
- **D4 — Config stays YAML**, same schema as `config.yaml`; byte-identical defaults test.
- **D5 — Providers: trait + tiers + single-owner retry.** All 10 real kinds ported
  (deepseek, openai, openrouter, opencode-go, openai-compatible, orcarouter, ollama, vllm,
  llamacpp, gemini) + scripted `FakeProvider`. Provider quirks stay in provider modules.
- **D6 — Desktop data preserved.** Verified backup + tested one-time importer from per-project
  `state.sqlite3` (catalog `local.sqlite3` is disposable scheduling state). CLI `state/` out of
  scope. Old workspace untouched until validation passes.
- **D7 — PDF via HTTP-isolated bridge only.** No AGPL code in-tree (MinerU/BabelDOC stay remote).
- **D8 — No shims.** Python runs side-by-side purely as a comparison oracle, then is deleted whole.
- **D9 — GPUI-Kit 0.8 locked** during engine work; 0.9.1 upgrade is a separate change.
- **D10 — Prompts + i18n embedded** (`include_str!`), byte-compared; 50k-word eval rule for
  translation-affecting changes.

Open questions: Q2 eval corpus (1–2 PD novels ≥50k words, checksums recorded); Q3 PDF extraction
crate (MIT-compatible); Q4 updater signing/distribution; Q5 keyring round-trip per OS;
Q6 DOCX fidelity caveats; series UX (multi-volume split/group); style auto-accept thresholds;
chapter-worker concurrency defaults (benchmark 1/2/N); shipped-CLI question (owner call).

## 2. Behavior oracle (what to port, not discard)

Facade `pipeline/orchestrator.py:31 Orchestrator` (routing/locks only) →
`pipeline/{translation:43, translation_batch:74, context:13, title_translation:173, precision}` →
`agents/{translator:21, analyzer:26, synopsis, polisher, reviewer, review_loop/fixer/arbiter}` →
`glossary/store.py:301` (`upsert_term` → inserted/unchanged/**conflict**; `terms_in` relevant
subset) + `curator.py:37` (20 terms/pass, 5 evidence excerpts) →
`review_{workflow:37, chunks, rounds, checkpoint, autofix:29}` + `review/evidence.py:27` →
`storage/{protocol:20/30, sqlite:39, artifacts:15}` / `runstore.py:48` / `srt/store:28` /
`assemble/writer:35` / `ingest/tokens:22` (`cl100k_base` via tiktoken) / `i18n/data/tasks` (32 files).

Must-preserve invariants: `target None` = pending vs `""` = complete (MinerU exception);
completed batches/chapters skipped on resume; shadow-only review, autofix publishes via
pre-persisted recoverable index; usage/events append-once (`merge_usage_summaries`);
stable Segment/chapter/anchor/DOCX/`babeldoc_id` identities; short state lock vs long workflow
locks; secrets in env/keyring only; SRT independent of the book pipeline; stable
original-order merge; `max(40, …)` rolling context floor.

Known weaknesses to fix in Rust: static style brief (built once per run,
`translation.py:73`; refresh is fill-missing-only, `preparation.py:322-324`); curator scans all
chapters × segments per term (`curator.py:47-58`); book-centric project model; unbounded
in-memory run inputs (`translation.py:83`).

Native UI baseline: 13 screens in `apps/desktop/native/src/views/`, all calling `client.rs`
(loopback REST + polling; no WS client). Preserve screens and visual language; replace
`backend.rs`/`client.rs` with in-process `services.rs`/`events.rs` at integration time.

## 3. Target architecture

```text
Cargo.toml                        # workspace, centralized versions, committed Cargo.lock
crates/wenyi-core/                # lib: store/llm/document/glossary/style/pipeline/jobs
apps/desktop/native/              # GPUI UI + app wiring only
```

Rules: `wenyi-core` never depends on GPUI/server frameworks; UI holds no business logic or SQL;
no storage-trait hierarchy until a second backend exists; no internal REST.

Dependencies (verified 2026-11-10; re-verify at adoption): tokio 1.54.1 (features
`rt,rt-multi-thread,macros,sync,time`), reqwest 0.12 to match native (`default-features=false`,
rustls-only — kills the openssl-sys build failure), serde/serde_json, rusqlite 0.41.1
`bundled` (verify FTS5 before relying on it), thiserror, tracing, zip 9.x, quick-xml,
tiktoken-rs (parity-gated on multilingual corpus), keyring, aho-corasick 1.1.5 (staged, after
matcher parity fixtures), YAML spike (serde-saphyr 2.1.1 vs serde_yaml — pick by fixture
parity). Rejected until measured: rayon, sqlx/ORMs, Tantivy/ES, provider SDKs, actors.

Schema baseline (`workspace.sqlite3`, WAL, short tx only): projects → books → source_files /
chapters → segments + translations (`target NULL` = pending); segment_revisions; glossary_terms
(project-wide or per-book override, unique per project/langs/book/source) + aliases +
term_occurrences (incremental index); style_profiles + immutable style_versions + proposals;
jobs + job_events; usage_ledger (`batch_key` idempotency); review_runs + issues + publish_log
(autofix writes here first); artifacts; schema_migrations. Page by (book, ordinal); chapters
execute one at a time; matcher snapshot shared per glossary version.

Queues (Rust): `translate` (~128), `export` (~32), `curate` (~1024 dirty-term refs, low
priority); fixed workers (start 2/1/1); per-provider semaphores; same admission rules as
today's `admit_job` (per-chapter lanes, curator exception); wave snapshots
(glossary_version, style_version) merged in (book, chapter, segment) order; startup
`running` → `interrupted`; cancel at checkpoints. UI keeps coarse ≤4 Hz progress events.

## 4. Benchmarks and acceptance

Baselines B1–B12 vs pinned oracle before replacing anything (`FakeClient`): cold start, no-N+1
project list, <200 ms chapter page (10k chapters), <100 ms segment commit, ≤4 Hz progress,
bounded memory per stage, indexed glossary find vs full scan, incremental index updates,
queue saturation + kill-9 exactly-once usage, fake-HTTP provider suite, atomic byte-stable
export, binary size/build time. Corpora: PD novel ≥50k words (human quality before/after),
synthetic 10k-chapter project, CJK/multilingual matcher fixtures, 14k-page stress. Never
commit private books. Fake-model tests prove control flow, not quality.

## 5. Migration sequence

0. Golden corpus + baselines (no Rust yet). 1. Workspace + config parity + store skeleton.
2. Documents, EPUB-first (round-trip goldens). 3. LLM (all 10 providers) + bounded jobs.
4. One-book translation parity. 5. Series/style/curator. 6. Review/proofread/export parity.
7. Native in-process integration; remove Python reference. Each slice states its oracle-diff
gate; no destructive migration or Python removal before replacement + recovery tests pass.

Deferred: multi-user/cloud, plugins, embedded inference, auto-rewrite of completed chapters,
visual redesign, toolkit upgrade, required FTS5, web lookup, native PDF engine.
