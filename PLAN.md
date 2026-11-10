# Wenyi cleanup & Rust-rewrite plan

**Date:** 10 November 2026 · **Oracle pin:** `main@39068fcc` (user WIP excluded) · **Providers:** all 10 real kinds + fake

Target: one native Rust frontend (`apps/desktop/native`) + one Rust backend (`crates/wenyi-core`).
Python core/backend/desktop-sidecar stay **temporarily as read-only behavioral reference**; everything
web/CLI/Tauri/JS goes now. No backend implementation in this cleanup.

## Remain

| Path | Why |
|---|---|
| `apps/desktop/native/` | The product UI. Zero `path=`/tauri/JS deps (verified). |
| `apps/desktop/backend/` (`wenyi_desktop`) | Sidecar the native UI spawns today (`native/src/backend.rs:47-53`); read-only ref. |
| `packages/core/` (`wenyi_core`) | Engine oracle: pipeline/agents/glossary/ingest/assemble/llm/review/storage. Sidecar needs it (`desktop_sidecar.py:185-188`). |
| `packages/backend/` (`wenyi_backend`) | Sidecar's HTTP layer (`wenyi_desktop/main.py:11-13`, `local_runtime.py:285`). Zero hard PG/Redis/arq imports (verified). |
| `scripts/desktop_sidecar.py`, `scripts/desktop_entry.py` | Sidecar build + entry. |
| `scripts/check_backend_packages.py`, `scripts/check_wheel_resources.py` | Wheel checks for the 3 remaining packages (profiles trimmed). |
| `config.yaml`, `.env.example` (trimmed) | Config oracle + key-name contract. |
| `docs/{architecture,pipeline,configuration,cli,desktop}.md` + `docs/zh/` mirrors | Behavior oracle until ported. `cli.md` becomes headless-spec. |
| `docs/roadmap-custom.md` | Live fork log; moving-oracle risk marker, never a spec. |
| `docs/RUST-REWRITE.md` + `docs/zh/RUST-REWRITE.md` (new) | This plan's consolidated migration doc (replaces `docs-next/`). |
| `README.md`, `AGENTS.md`, `CONTRIBUTING.md` + ZH mirrors | Rewritten for the new layout. |
| `.github/workflows/{tests,desktop-native}.yml` | Trimmed CI: Python-ref tests, native build/test. |

## Removed

| Path | Evidence it is safe |
|---|---|
| `apps/web/`, `apps/desktop/frontend/`, `packages/ui/`, `packages/shared-schema/`, `package.json`, `pnpm-*.yaml` | Closed JS graph: hosts → `@wenyi/ui` → `shared-schema` → generated from `apps/api`. Native has zero JS/TS imports (only doc-comment parity notes). One Python consumer: `backend/tests/test_settings_models_list.py:139-142` reads `api.d.ts` — test trimmed in same change. |
| `apps/api/` + `apps/api/tests` | `wenyi_api` imported only by itself + its tests (`apps/api/tests/*`); sidecar `EXCLUDED_MODULES` asserts absence. Removes 100% of `psycopg/arq/redis` production imports. |
| `packages/cli/` | `wenyi_cli` has zero production consumers (only `core/tests/*` ~20 files + `scripts/cli_entry.py`). Sidecar excludes it. Core tests importing CLI are deleted/trimmed (list below). |
| `apps/desktop/src/`, `build.rs`, `tauri.*.conf.json`, `icons/` | Tauri host. Native crate has zero tauri deps (separate lockfiles, no workspace). Icon asset ref in `tauri.conf.json` dies with it. |
| `docker/`, `deploy/`, `.dockerignore` | Web-only hosting; zero CI refs (CI uses GH service containers). `test_nginx_proxy.py` (Docker-gated, never runs in CI) + `Dockerfile` param in `test_release_packaging.py:291` deleted in same change. |
| `scripts/{desktop.mjs,desktop.test.mjs,desktop-ui.sh,desktop_updates.py,desktop_pe.py,release_version.py,package_release.py,cli_entry.py}` | Tauri/CLI/packaging-only; ref table in audit. `node`/`scripts/desktop.mjs` refs removed from CI. |
| `.github/workflows/{build.yml,desktop.yml}` | CLI PyInstaller + Tauri packaging; replaced by `desktop-native.yml` (native `cargo build/test`). |
| `docs/{web.md,design/*}` + ZH mirrors | Web/React-only. Two loading/notification rules extracted into `RUST-REWRITE.md` Ch.7. |
| `docs-next/` (all 20 files) | Consolidated into `docs/RUST-REWRITE.md` (+ZH); `19-rust-native-audit.md` corrections folded in. Git history keeps originals. |
| Core tests (deleted): `test_cli.py`, `test_cli_context.py`, `test_cli_progress.py`, `test_model_commands.py`, `test_language_policy_cli.py`, `test_nginx_proxy.py` | CLI-surface or Docker-only. |
| Core tests (trimmed): `test_bilingual.py`, `test_docx.py`, `test_i18n.py`, `test_input_preparation.py`, `test_language_detection_errors.py`, `test_llm_connectivity.py`, `test_metadata_language.py`, `test_pdf_export_defaults.py`, `test_pdf_support.py`, `test_precision_configuration.py`, `test_precision_pipeline.py`, `test_srt.py`, `test_timing.py`, `test_release_packaging.py`, `test_architecture_boundaries.py`, `test_storage_injection.py` | Remove `typer`/`CliRunner`/`wenyi_cli` imports + CLI-invoking cases; keep engine assertions via direct API calls. |
| Backend tests: `test_settings_models_list.py` schema-crosscheck block | Reads deleted `api.d.ts`; keep backend-side OpenAPI assertions. |
| `pyrightconfig.json`, `deploy/*`, root `package.json` scripts | Dead with the above. |

## Manifest / CI edits

- `pyproject.toml`: workspace members 5→3 (`core`, `backend`, `desktop/backend`); root `dependencies=["wenyi-cli"]` → `[]`; drop `wenyi-cli`/`wenyi-api` sources; `testpaths` → core/backend/desktop tests; `pythonpath` → `packages/core`.
- `uv.lock`: regenerate via `uv lock`.
- `tests.yml`: drop PG/Redis services, integration job, CLI smoke, web job, deleted-package builds; ruff scope → `packages/core packages/backend apps/desktop/backend scripts`.
- `desktop.yml` → `desktop-native.yml`: native `cargo build/test` (+ existing sidecar smoke), no Tauri/Node.
- `AGENTS.md`/`README.md`/`CONTRIBUTING.md` (+ZH): new layout, no web/CLI/Tauri commands, Rust rules stub.
- `scripts/check_backend_packages.py`: profiles → `core`/`shared`/`desktop` only.
- `.env.example`: keep `*_API_KEY` names, drop DB/Redis/CORS vars.

## Target architecture

```text
apps/desktop/native/        # GPUI UI + services.rs/events.rs (replaces backend.rs/client.rs later)
crates/wenyi-core/          # Rust engine: store/llm/document/glossary/style/pipeline/jobs
reference/python/           # (later move) core/backend/desktop-sidecar as read-only oracle
```

Two Rust packages only. `wenyi-core` never depends on GPUI/Tauri/server frameworks.
Dependency shortlist (verified 2026-11-10): tokio 1.54.1, reqwest 0.12 to match native (`default-features=false`,
rustls-only — kills the openssl-sys build failure), serde/serde_json, rusqlite 0.41.1
`bundled` (verify FTS5 before relying on it), thiserror, tracing,
zip 9.x (re-evaluate), quick-xml, tiktoken-rs (parity-gated), keyring, aho-corasick 1.1.5 (staged),
YAML: serde_yaml 0.9 (spike winner 2026-11-10; saphyr parsed identically but heavier). Reject: rayon/sqlx/Tantivy/SDKs/actors.

## Migration sequence (after cleanup)

Status 2026-11-10: Slice 0 ✅ (8 deterministic goldens + baselines in
`tests/golden/`) and Slice 1 ✅ (`crates/wenyi-core`: config parity, series
store, redaction, dep guard, CI `core` job) are complete. Slice 2 ✅
(documents: text/md reader + segmentation, EPUB read path, template assembly
round-trip goldens in 4 export modes, HTML reader, SRT parser; oracle CJK-NAV
mojibake deliberately not reproduced). Slice 3 ✅ (`llm/`: 18 operations,
routing fingerprints, all 11 kinds incl. fake, reqwest transport, single-owner
retry, budgets/quotas, usage ledger, `FakeClient`; `jobs/`: bounded queues,
admission lanes, cancel/recovery, curator sweep; 7 LLM goldens + 19 fake-HTTP
provider tests + 9 jobs tests). Slice 4 foundation ✅ (2026-11-11): `i18n/`
(embedded prompts, byte-identical policy fingerprints), `glossary/`
(matcher parity, upsert/conflict store), schema migration 2. Engineering
hardening ✅ (2026-11-11, per `WENYI_RUST_ENGINEERING.md`): 4-way audit +
fixes — `forbid(unsafe_code)`, unified canonical_json/hex/ruby-strip,
LazyLock i18n JSON, zip deflate-only + tracing removed (tree 354→304),
async retry sleep, jobs shutdown-drain/cancel/admit-race/grace/IMMEDIATE,
typed-error holes closed, schema/type lag closed; 96 tests green,
clippy `-D warnings` clean. Slice 4 wiring ✅ (2026-11-11):
`pipeline_agents` (translator/analyzer/synopsizer/polisher/extractor/titles
with alignment + polish-continue + first-occurrence align),
`pipeline` (prepare → digests → synopsis → chapters → titles, rolling
context, checkpoints, resume) — `translate_txt.json` golden passes
byte-exact (13/13 calls, usage, targets, events); forced-rerun parity
verified against oracle (1 glossary.extract); 99 tests green, clippy/fmt
clean. Next: Slice 5 (series/style/curator).

0. Golden corpus + baselines B1–B12 vs pinned oracle (`FakeClient`). No private books.
1. Workspace + config parity + store skeleton (`workspace.sqlite3`, series schema, backup API).
2. Document foundation, EPUB-first (round-trip goldens vs oracle).
3. LLM (all 10 providers + fake) + bounded jobs (queue caps, exactly-once `batch_key` usage).
4. One-book translation parity → 5. series/style/curator → 6. review/proofread/export parity →
   7. native in-process integration → remove Python reference.
