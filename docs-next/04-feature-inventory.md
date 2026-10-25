# Feature inventory — Wenyi Rust-native rewrite

Exhaustive, path-evidenced list of what exists today and what happens to it in
the Rust rewrite. Every row cites a repo-relative path with a symbol or line
number. Status vocabulary: `existing-working`, `existing-incomplete`,
`preserve`, `improve`, `new`, `remove`, `unverified`. Disposition (the part
after `→`) follows the shared target verdicts (port behavior to Rust; remove
web/TS/Python/Tauri after parity) unless a row says otherwise.

## 1. UI screens (13 pages, 14 routes)

Source of truth: `packages/ui/src/routes/manifest.tsx` — `pageLoaders` (:56,
13 loaders), `routeEntries` (:79, 14 entries), `navEntries` (:125, 13 entries).
Feature components live in `packages/ui/src/features/` (`contents`,
`dashboard`, `events`, `export`, `glossary`, `progress`, `project-create`,
`proofreading`, `review`, `settings`, `style`, `subtitles`).

| Screen | Route | Evidence | Status |
|---|---|---|---|
| Dashboard (project list) | `/` | `manifest.tsx:81`, `features/dashboard/Dashboard` | existing-working → preserve as UX spec |
| Create project | `/projects/new` (child of `/`) | `manifest.tsx:83`, `features/project-create/CreateProject` | existing-working → preserve as UX spec |
| Translation overview / progress | `/projects/:pid` | `manifest.tsx:87`, `features/progress/ProgressPage` | existing-working → preserve as UX spec |
| Glossary | `/projects/:pid/glossary` (book only, `isVisibleFor`, `manifest.tsx:103`) | `manifest.tsx:88` | existing-working → preserve as UX spec |
| Style / synopsis | `/projects/:pid/style` (book only) | `manifest.tsx:89`, `features/style/StylePage` | existing-working → preserve as UX spec |
| Contents | `/projects/:pid/contents` (book only) | `manifest.tsx:90` | existing-working → preserve as UX spec |
| Whole-book review | `/projects/:pid/review` (book only) | `manifest.tsx:91`, `features/review/ReviewPage` | existing-working → preserve as UX spec |
| Proofreading (chapter + chapter/segment variants) | `/projects/:pid/proofreading`, `/projects/:pid/proofreading/:ci` | `manifest.tsx:92-93` | existing-working → preserve as UX spec |
| Project settings | `/projects/:pid/settings` | `manifest.tsx:94` | existing-working → preserve as UX spec |
| Subtitle editor | `/projects/:pid/subtitles` (srt only) | `manifest.tsx:95`, `features/subtitles/SubtitlesPage` | existing-working → preserve as UX spec |
| Export | `/projects/:pid/export` | `manifest.tsx:96`, `features/export/ExportPage` | existing-working → preserve as UX spec |
| Event log | `/projects/:pid/events` | `manifest.tsx:97`, `features/events/EventsPage` | existing-working → preserve as UX spec |
| Interface settings | `/settings` (+ `/settings/:section` redirect) | `manifest.tsx:85-86` | existing-working → preserve as UX spec |

Supporting UI transport (all `remove` after parity — single binary has no
HTTP/WS product surface):

| Item | Evidence | Status |
|---|---|---|
| REST client, 52 `request<` call sites in one `api` object | `packages/ui/src/lib/api.ts:52`, `api.ts:53-281` | existing-working → remove (replaced by in-process calls; method list is the operation catalog for the GUI crate, see 05) |
| WS progress hook, reconnect with capped exponential backoff | `packages/ui/src/lib/ws.ts:19` (`onclose` → `setTimeout(connect, …)`, `backoff *= 2` capped 5 s); event kinds `snapshot\|stats\|state\|progress\|batch\|chapter\|term\|pipeline\|log` (`ws.ts:7`) plus `final` (`ws.ts:111`) | existing-working → remove (replaced by in-process event bus; kind list is the event-taxonomy spec) |
| `PlatformServices` abstraction (`configurePlatform`) | `packages/ui/src/platform.ts:34`, `:87` | existing-working → preserve concept (native-capability seam in the GUI crate), remove code |
| Bearer-token auth + `TokenMiddleware` (all paths except `/health`, `/ws/`) | `packages/backend/wenyi_backend/application.py:38` | existing-working → remove (no network product; single-user desktop needs no bearer tokens) |

## 2. CLI commands (Typer app, `packages/cli/wenyi_cli/cli.py:16` `create_app`)

| Command | Evidence | Status |
|---|---|---|
| `parse` — parse without AI | `commands/workflows.py:31` | existing-working → preserve as CLI spec |
| `translate` — full pipeline incl. SRT branch and `--translation-mode` (`standard`/`best_of_three`) | `commands/workflows.py:324`, `TranslationMode` (`workflows.py:25`), `--translation-mode` (`workflows.py:355`) | existing-working → preserve as CLI spec |
| `prepare` — parse, detect language, analyze style/terms, prescan | `commands/workflows.py:390` | existing-working → preserve as CLI spec |
| `review` — evidence review + optional autofix | `commands/workflows.py:400` | existing-working → preserve as CLI spec |
| `language-policy` — preview language operations without model calls | `commands/inspection.py:23` | existing-working → preserve as CLI spec |
| `languages` — list built-in languages (**experimental** per docstring) | `commands/inspection.py:53` | existing-incomplete → improve (stabilize or cut in the Rust CLI) |
| `status` — chapter progress + glossary stats | `commands/inspection.py:68` | existing-working → preserve as CLI spec |
| `assemble` — export from existing state, no model calls | `commands/inspection.py:98` | existing-working → preserve as CLI spec |
| `report` — regenerate `report.json` without model calls | `commands/inspection.py:155` | existing-working → preserve as CLI spec |
| `glossary list` / `conflicts` / `resolve` | `commands/glossary.py:22`, `:51`, `:79` | existing-working → preserve as CLI spec |
| `models list` / `explain` / `check` / `ping` / `migrate-config` / `migrate-usage` | `model_commands.py:29`, `:65`, `:75`, `:93`, `:135`, `:156` | existing-working → preserve as CLI spec (`migrate-*` become `import-legacy-state` equivalents; see 06) |

## 3. Pipeline stages

`Orchestrator.ALL_STEPS = ("translate", "review", "report", "assemble")`
(`packages/core/wenyi_core/pipeline/orchestrator.py:34`); thin façade
(`orchestrator.py:30`, must not import agents/ingest/glossary/assemble per
`AGENTS.md` architecture boundary).

| Stage | Service + evidence | Status |
|---|---|---|
| prepare (parse, locate state, init under book lock, language detect, style/term analysis, prescan) | `PreparationService` (`pipeline/preparation.py:70`); facade `Orchestrator.prepare` (`orchestrator.py:59`) | existing-working → preserve, port algorithms |
| translate (chapter loop, batch glossary extraction, precision `best_of_three` branch, history update) | `TranslationService` (`pipeline/translation.py:43`); precision branch (`translation.py:327`) | existing-working → preserve, port algorithms |
| review (evidence sessions, shadow revisions, blind rechecks) | `ReviewService` (`pipeline/review_workflow.py:37`, `run_session` `:225`); facade `run_review` (`orchestrator.py:139`) | existing-working → preserve, port algorithms |
| review autofix (publish via recoverable index only) | `ReviewAutofixService` (`pipeline/review_autofix.py`, wired `orchestrator.py:54`) | existing-working → preserve, port exactly incl. shadow-only invariant |
| report | `ReportService` (`pipeline/finalization.py:27`); facade `run_report` (`orchestrator.py:207`) | existing-working → preserve, port |
| assemble / export | `AssemblyService` (`pipeline/finalization.py:59`, `assemble_outputs` `:65`, `assemble_live` `:140`, `assemble_snapshot` `:173`); facade `run_assemble` (`orchestrator.py:220`) | existing-working → preserve, port |
| run/step routing (`run`, `run_steps`, `run_all`) | `orchestrator.py:91`, `:262`, `:397` | existing-working → preserve, port |
| RunStore (dirs, locks, manifest paths, timing) | `RunStore` (`pipeline/runstore.py:48`); `lock` `:71`, `state_lock` `:77`, `assemble_lock` `:99` | existing-working → preserve, port (lock semantics are invariants, see `AGENTS.md` state section) |
| SRT translation (independent of book Orchestrator, no prescan/glossary/polish/review) | `srt/translate.py:180` `translate_srt`, `srt/store.py:28` `SrtRunStore` | existing-working → preserve, port as-is |

## 4. LLM providers and routing

Registry: `ProviderSpec` (`packages/core/wenyi_core/llm/registry.py:55`) with 10
kinds — deepseek, openai, openrouter, opencode-go, openai-compatible, orcarouter,
ollama, vllm, gemini, fake (`packages/core/wenyi_core/llm/registry.py:113-139`).
Tiers `("strong", "cheap", "fast")` (`llm/operations.py:14`); `RoutedLLMClient`
(`llm/router.py:27`); route resolution (`llm/routing.py:112` `resolve_routes`);
unified retry (`llm/retrying.py:359` `provider_retry`, provider SDK retries must stay
off per `AGENTS.md`).

| Provider | Evidence | Status |
|---|---|---|
| openai | `llm/providers/openai.py:49` `OpenAIClient` | existing-working → preserve protocol knowledge |
| openai_compatible (generic base + concrete) | `llm/providers/_openai_compatible.py:91`, `openai_compatible.py:82` | existing-working → preserve protocol knowledge |
| deepseek | `llm/providers/deepseek.py:82` | existing-working → preserve protocol knowledge |
| gemini | `llm/providers/gemini.py:141` | existing-working → preserve protocol knowledge |
| ollama (local) | `llm/providers/ollama.py:8` | existing-working → preserve protocol knowledge |
| openrouter | `llm/providers/openrouter.py:54` | existing-working → preserve protocol knowledge |
| opencode_go | `llm/providers/opencode_go.py:57` | existing-working → preserve protocol knowledge |
| orcarouter | `llm/providers/orcarouter.py:9` | existing-working → preserve protocol knowledge |
| vllm (self-hosted OpenAI-compatible) | `llm/providers/vllm.py:8` | existing-working → preserve protocol knowledge |
| fake (tests, no network) | `llm/providers/fake.py:37` `FakeClient` | existing-working → preserve concept (fixture/fake client drives parity tests in 06) |
| Credential hygiene (control-char reject, full-snapshot redaction) | `llm/credentials.py:7`, `:13` | existing-working → preserve, port exactly |

## 5. Input formats (readers) and output formats (writers)

CLI-supported inputs are declared in help text: `EPUB / FB2 / TXT / Markdown /
HTML / PDF / DOCX / SRT` (`commands/workflows.py:325`). Export formats:
`Literal["epub", "txt", "html", "markdown", "pdf", "docx", "srt"]`
(`packages/backend/wenyi_backend/schemas.py:210`); writer dispatch
(`packages/core/wenyi_core/assemble/writer.py:35` `assemble`).


## 6. Glossary

| Item | Evidence | Status |
|---|---|---|
| SQLite store + schema | `glossary/store.py:218` `GlossaryStore`, `:240` `initialize_schema` | existing-working → preserve, port schema |
| Occurrence matcher (source patterns, merged counts) | `glossary/store.py:190` `GlossaryOccurrenceMatcher`, `:134` `source_matches_text` | existing-working → preserve, port exactly |
| Extractor (translation-evidence-backed candidates) | `glossary/extractor.py:45` `GlossaryExtractor` | existing-working → preserve, port |
| Resolver (conflict decisions) | `glossary/resolver.py:11` `resolve` | existing-working → preserve, port |
| Read-only snapshot for review (`load_terms_readonly`, WAL-inclusive, formal DB untouched) | `glossary/store.py:252` | existing-working → preserve, port exactly (state invariant) |

## 7. Review and evidence

| Item | Evidence | Status |
|---|---|---|
| Reviewer (cheap tier, omission/addition/mistranslation/glossary/pronoun checks) | `agents/reviewer.py:33` | existing-working → preserve, port prompts + parsing |
| Evidence-gated action loop (request-evidence/final, ref validation) | `agents/review_actions.py:118`, `:20` `validate_evidence_refs` | existing-working → preserve, port |
| Fixer (complete-paragraph provisional patches, cannot self-publish) | `agents/review_fixer.py:123`, `:31` `ProvisionalPatch` | existing-working → preserve, port |
| Conflict arbiter (read-only recommendations) | `agents/review_arbiter.py:20` | existing-working → preserve, port |
| Block loop driver | `agents/review_loop.py:24` | existing-working → preserve, port |
| Book evidence index (term occurrences, segment/book context, ref registry) | `review/evidence.py:27` `BookEvidenceIndex` | existing-working → preserve, port algorithms |
| Session state machine (rounds, overlays, digests, fix plans) | `review/session.py:110` `ReviewSessionState`, `:26` `review_overlay_digest` | existing-working → preserve, port |
| Run store, outcomes, contracts, conflicts | `review/run_store.py:17`, `review/models.py:27`, `review/contracts.py:13`, `review/conflicts.py`, `review/autofix_models.py` | existing-working → preserve, port |
| Annotation aligner (marked-source validation, deterministic fallback) | `agents/annotation_aligner.py:297`, `:166` `validate_marked_target`, `:247` `fallback_alignment` | existing-working → preserve, port |
| Markup primitives (anchors, annotations, segments, ruby, contracts) | `markup/anchors.py`, `markup/annotations.py`, `markup/segments.py`, `markup/ruby.py`, `markup/contracts.py` | existing-working → preserve, port |

## 8. Agents (remaining)

| Agent | Evidence | Tier | Status |
|---|---|---|---|
| `Agent` base (phase policy, `_ask_text`) | `agents/base.py:24` | — | existing-working → preserve as trait |
| Translator (N-in/N-out alignment, count-mismatch retry, per-paragraph fallback) | `agents/translator.py:21` | strong | existing-working → preserve, port |
| TitleTranslator | `agents/title_translator.py:19` | (route) | existing-working → preserve, port |
| Analyzer (style guide, characters, seed terms) | `agents/analyzer.py:26` | strong | existing-working → preserve, port |
| Synopsizer (chapter digests, book synopsis) | `agents/synopsis.py:18` | fast | existing-working → preserve, port |
| Polisher (paragraph-count-preserving, cacheable prefix) | `agents/polisher.py:15` | strong | existing-working → preserve, port |
| PrecisionAgent (stable prefix, drafts-as-data synthesis) | `agents/precision.py:40` (+ `pipeline/precision.py`, `precision_records.py`) | — | existing-working → preserve, port |

## 9. Export, updates, credentials, OS integration

| Item | Evidence | Status |
|---|---|---|
| Export snapshot isolation + atomic publish (temp file + rename; cancel creates nothing) | `storage/protocol.py:53` `create_export_snapshot`; `AGENTS.md` state section | existing-working → preserve, port exactly |
| Desktop export planning (`plan_export`, history, save-to-picked-path) | `apps/desktop/backend/wenyi_desktop/routers/desktop_export.py:37` `plan_export` (scout-verified) | existing-working → preserve behavior |
| Native save dialog + `native_export_save` command | `apps/desktop/src/native_export.rs:426` (`NativeDialog`), `:851`; 7 commands registered `apps/desktop/src/main.rs:226` (scout-verified) | existing-working → preserve behavior, new implementation (no Tauri) |
| Native file-drop upload | `apps/desktop/src/native_drop.rs`; commands `native_drop_release`/`native_drop_upload` (`main.rs:226`) | existing-working → preserve behavior |
| Signed updater (status/check/download/install) | `tauri-plugin-updater = "2"` (`apps/desktop/Cargo.toml:26`); `apps/desktop/src/updates.rs`; backend `UpdateGuard` (`wenyi_desktop/updates.py:16`, scout-verified) | existing-working → preserve behavior, new implementation |
| Single-instance, tray, sidecar spawn, watch-parent + `WENYI_READY` protocol | `apps/desktop/src/instance.rs`, `background.rs`, `process.rs`; `wenyi_desktop/desktop.py:61` `watch_parent`, `:101` `WENYI_READY` | existing-working → preserve behavior (single-instance + tray are desktop must-haves on Linux) |
| CredentialStore (system keyring first, memory-only fallback, never plaintext) | `wenyi_desktop/local_credentials.py:70` `CredentialStore`, `:41` `system_keyring`, `:406` `credential_store()`; routers `routers/credentials.py`, `routers/external_credentials.py` (scout-verified) | existing-working → preserve, port exactly (keyring crate, see 05) |
| Local runtime (in-process asyncio queues, progress hub, workspace lock) | `wenyi_desktop/local_runtime.py:194` `LocalRuntime`, `:90` `ProgressHub`, `:43` `WorkspaceLock` | existing-working → preserve architecture (tokio tasks + broadcast bus, see 05) |
| Per-project `SqliteStorage`, catalog `local.sqlite3`, file dialogs via `rfd 0.15` | `storage/sqlite.py:34` `SqliteStorage`; `rfd = "0.15"` (`apps/desktop/Cargo.toml:24`) | existing-working → preserve |

## 10. i18n, prompts, configuration

| Item | Evidence | Status |
|---|---|---|
| 16 translation-language JSONs + registry (en, en-GB, en-US, es, fr, it, ja, ko, pt, pt-BR, pt-PT, ru, vi, zh, zh-Hant, de) | `packages/core/wenyi_core/i18n/data/languages/` (directory listing) | existing-working → preserve, embed as resources |
| 34 task prompt templates (translator, reviewer, analyzer, polisher, precision, SRT, titles, …) | `packages/core/wenyi_core/i18n/data/tasks/` (34 files) | existing-working → preserve, embed as resources |
| UI locales (English + Simplified Chinese only) | `packages/ui/src/i18n/locales/en.ts`, `zh-CN.ts` | existing-working → preserve (GUI ships en + zh-CN first) |
| Prompt rendering helpers (glossary render, numbered pairs, annotation contexts) | `agents/prompts.py:10`, `:60`, `:27` | existing-working → preserve, port |
| Default config template + root `config.yaml` | `core/config.py` `_DEFAULT_CONFIG_YAML` (`AGENTS.md`); `config.yaml` (repo root) | existing-working → preserve (same YAML schema, byte-identical defaults per D4 in doc 16) |
| Backend routers as operation catalog (14 routers: health/strategies/projects/configuration/settings/report/subtitles/chapters/glossary/review/style/export/events/ws) | `packages/backend/wenyi_backend/routers/` (directory listing) | existing-working → remove code, keep as GUI-operation checklist |

## 11. Explicitly removed after parity

| Item | Evidence it exists | Status |
|---|---|---|
| `apps/web` React host | `apps/web/` | remove |
| `packages/ui` shared React UI + `apps/desktop/frontend` | `packages/ui/src/App.tsx` | remove (13-screen inventory above is the UX spec) |
| `packages/shared-schema` generated TS types | `packages/shared-schema/` | remove (Rust types are the schema) |
| `apps/api` (PG pool, arq queues, `wenyi:workflows`, 13-table `schema.sql`) | `apps/api/wenyi_api/` | remove (no web product; no PG/Redis) |
| `docker/`, `deploy/` (pg16/redis7/api/worker/web, nginx) | `docker/`, `deploy/` | remove |
| All Python product code (`wenyi-core`, `wenyi-cli`, `wenyi-backend`, desktop backend) | `packages/`, `apps/*/backend` | remove after behavioral parity (fixtures keep them honest, see 06) |
| Tauri shell (keep patterns only: commands, updater wiring, tray behavior) | `apps/desktop/src/*.rs`, `tauri = "2"` (`apps/desktop/Cargo.toml:16`) | remove shell, salvage patterns |

## 12. Unverified (need confirmation during implementation)

- Exact REST method count semantics: 52 `request<` call sites counted in
  `packages/ui/src/lib/api.ts`; whether each maps 1:1 to a GUI operation needs
  a pass over `api.ts:53-281` during GUI design.
- `assemble/export_view.py` `ExportViewStore` internals (file listed, not read).
- MinerU token-acquire edge cases beyond `CredentialStore.resolve_mineru_token`.
- Windows/macOS-only behaviors (console setup in `commands/bootstrap.py`,
  `windows-sys` file-system feature in `apps/desktop/Cargo.toml:36`) — Linux
  is the first-class target; others are best-effort.
- `packages/core/wenyi_core/events.py` and `timing.py` internals (files listed,
  not read) — presumed portable; confirm when porting telemetry.

## Sources

- `packages/ui/src/routes/manifest.tsx:56-143`
- `packages/ui/src/lib/api.ts:52-281`, `packages/ui/src/lib/ws.ts:7-115`, `packages/ui/src/platform.ts:34-87`
- `packages/cli/wenyi_cli/cli.py`, `packages/cli/wenyi_cli/commands/workflows.py`, `commands/inspection.py`, `commands/glossary.py`, `model_commands.py`
- `packages/core/wenyi_core/pipeline/orchestrator.py`, `preparation.py`, `translation.py`, `review_workflow.py`, `review_autofix.py`, `finalization.py`, `runstore.py`
- `packages/core/wenyi_core/llm/registry.py`, `operations.py`, `router.py`, `routing.py`, `retrying.py`, `credentials.py`, `providers/`
- `packages/core/wenyi_core/ingest/`, `packages/core/wenyi_core/assemble/writer.py`, `assemble/epub_writer.py`, `assemble/docx_writer.py`, `assemble/html_writer.py`, `assemble/pdf_writer.py`, `assemble/text_writer.py`, `assemble/srt_writer.py`
- `packages/core/wenyi_core/agents/`, `packages/core/wenyi_core/review/`, `packages/core/wenyi_core/markup/`, `packages/core/wenyi_core/glossary/`, `packages/core/wenyi_core/srt/`, `packages/core/wenyi_core/storage/protocol.py`, `packages/core/wenyi_core/pdf_bridge/client.py`, `packages/core/wenyi_core/i18n/data/`
- `packages/backend/wenyi_backend/application.py`, `routers/`, `schemas.py:210`
- `apps/desktop/backend/wenyi_desktop/local_runtime.py`, `local_credentials.py`, `desktop.py`, `updates.py`, `routers/`
- `apps/desktop/Cargo.toml`, `apps/desktop/src/`
- `AGENTS.md` (boundary, state-invariant, credential sections)

## Open questions

See 16-decisions-and-open-questions.md. Largest inventory risks: native PDF
generation story ( §11), stabilizing `languages` experimental surface, and
confirming the §12 unverified rows before freezing GUI scope.
