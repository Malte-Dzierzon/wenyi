# Codebase Audit

Per-subsystem verdicts for the Rust rewrite. Verdict vocabulary: **port**
(reimplement behavior in Rust), **adapt** (reuse logic with platform changes),
**reuse-as-spec** (behavioral reference only, then delete), **remove-later**
(delete after parity, no port). All verdicts follow the shared target verdicts
(PORT behavior; REMOVE web stack/TS/Python/Tauri shell) unless noted.

## Audit table

| Subsystem | What / how (key symbols) | Dependents | Verdict + reason | Risk |
|---|---|---|---|---|
| `packages/core/wenyi_core/pipeline/` (orchestrator, preparation, translation*, review_*, autofix_*, finalization, runstore) | Staged book pipeline; `Orchestrator` thin facade (`pipeline/orchestrator.py`), stage services own logic; `RunStore` state access; manifest-last commits, shadow-only review, recoverable autofix index (`AGENTS.md` invariants) | CLI, backend workers | **port** — the product's core value; algorithms + resume/commit semantics must survive exactly | High: subtle resume/accounting bugs if stage contracts drift (doc 07) |
| `packages/core/wenyi_core/agents/` + `prompts.py` + `i18n/data/` | Model task loops (translator, polisher, reviewer, review_loop/fixer/arbiter, analyzer, synopsis, title_translator, annotation_aligner, precision); English prompt templates + 16 language JSONs as resources | pipeline stages | **port** — prompts + i18n data embedded as resources; loop control flow ported | High: prompt drift changes quality silently; needs golden tests (doc 14) |
| `packages/core/wenyi_core/llm/` + `providers/` | `LLMClient` base, `Provider/Model/Route/Quota/Budget` config, `RoutedLLMClient` via `factory.py`, strong/cheap/fast tiers (`router.py`), unified `retrying.py`, `operations.py` registry; 10 provider kinds (`packages/core/wenyi_core/llm/registry.py:113-139`): deepseek, openai, openrouter, opencode-go, openai-compatible, orcarouter, ollama, vllm, gemini, fake | agents, pipeline | **port** — provider protocol knowledge + routing/retry semantics; single retry owner | Med: per-provider quirks; keep `FakeClient` equivalent for tests |
| `packages/core/wenyi_core/ingest/` | Readers (epub incl. `epub_chapters/package/toc/layout`, fb2, html, text, docx, pdf, MinerU `pdf_to_html`, `pdf_babeldoc`, srt) + `models.py` Segment/Chapter/Document + `segmenter.py`, `source_hash.py`, `tokens.py` (tiktoken) | preparation, assemble | **port** — format semantics + stable IDs + source-hash binding | Med: EPUB/PDF edge cases; tokenizer parity (tiktoken → Rust equivalent) |
| `packages/core/wenyi_core/assemble/` | Writers (epub incl. navigation/presentation/resources, docx blocks/numbering/styles, html_*, text, srt, pdf) + `export_view.py` `ExportViewStore`, `report.py`, `policy.py`, `about.py` | finalization, export routes | **port** — EPUB round-trip fidelity is the top format priority | Med: DOCX/EPUB style edge cases need golden files |
| `packages/core/wenyi_core/markup/` + `document_styles/docx.py` | Deterministic anchors/annotations/ruby/segments (`markup/`), pure DOCX style policy; no pipeline/LLM deps | ingest, assemble, annotations aligner | **port** as pure functions first — easiest Rust win, pin with unit tests | Low |
| `packages/core/wenyi_core/glossary/` | `GlossaryStore` SQLite + conflicts, extractor, resolver | translation, review, glossary routes | **port** — schema + matcher semantics preserved into app SQLite | Med: matching edge cases, WAL-snapshot read rule |
| `packages/core/wenyi_core/review/` | `models.py`, `contracts.py`, `evidence.py` `BookEvidenceIndex`, `conflicts.py`, `session.py`, `run_store.py`, `autofix_models.py` | review pipeline + agents | **port** — evidence algorithms are a differentiator | Med |
| `packages/core/wenyi_core/srt/` + `postprocess/` + `pdf_bridge/` | SRT sliding-window translate/store/policy; export_text/punct; HTTP-only BabelDOC client | SRT routes, export | **port** (SRT lightweight path stays glossary/polish/review-free); **adapt** pdf_bridge to Rust HTTP client, keep process isolation | Low |
| `packages/core/wenyi_core/storage/` + `config.py` | `Storage`/`ArtifactStorage` protocols, file/sqlite/file-artifacts/locks adapters; `_DEFAULT_CONFIG_YAML` | all stages, all platforms | **adapt** — protocols become Rust traits; file backend dropped, SQLite backend becomes the only store; config template ported | Med: artifact-key `/`-separator + prefix-scan rules must carry over |
| `packages/cli/wenyi_cli` | Typer app (`cli.py`, `main.py`), commands (parse/translate/prepare/status/assemble/report/languages/language-policy, glossary, models) | end users (local) | **reuse-as-spec** — command surface documents headless behavior; not a shipped product | Low |
| `packages/backend/wenyi_backend` | HTTP contract: `create_app`, Bearer `TokenMiddleware`, 14 routers, services, emitters, WS relay | api + desktop backends, frontends | **reuse-as-spec** — routes/schemas/WS kinds define behavior to absorb in-process; no HTTP server in target | Med: route-by-route parity checklist (doc 04) |
| `apps/api/wenyi_api` | PG/Redis/arq adapters, `PostgresStorage`, 13-table `db/schema.sql`, queues `wenyi:workflows`/`wenyi:exports` | web deploys | **remove-later** — platform-specific, no desktop value | Low (isolated) |
| `apps/desktop/backend/wenyi_desktop` | SQLite catalog, per-project stores, asyncio runner, keyring creds, desktop-only routers | desktop app | **reuse-as-spec then remove** — catalog schema + runner semantics inform Rust design; no Python retained | Med: catalog recovery rules worth porting (doc 12) |
| `apps/desktop/src` (Tauri host) | single-instance, tray, sidecar spawn, dialogs, updater, drop/export grants | desktop app | **reuse-as-spec (patterns only)** — OS behaviors reimplemented in native Rust GUI (doc 10) | Med: updater/signing flow |
| `apps/web` + `apps/desktop/frontend` + `packages/ui` + `packages/shared-schema` | React hosts + 13-screen shared UI + generated `api.d.ts` | users (UI) | **reuse-as-spec (UX only)** — 13-screen inventory + workflows port to native GUI (doc 10); all TS deleted | Med: interaction details lost if spec is sloppy |
| Root `config.yaml`, `pyproject.toml`, `package.json`, `docker/`, `deploy/`, `scripts/` | defaults, workspace manifests, web images/compose, entry/packaging helpers | builds, deploys | **remove-later**; config defaults **port** as Rust defaults; packaging helpers inform the GUI/packaging design (doc 10) | Low |
| `.github/workflows/` | `tests.yml`, `build.yml`, `desktop.yml` | CI | **adapt** — replaced by Rust CI (doc 14); keep PG/E2E jobs until web deleted | Low |
| `docs/` (+`zh/`, `design/`) | Current-code guides + unimplemented proposals | developers | **reuse-as-spec selectively** — pipeline/architecture guides inform port; `design/` proposals are explicitly not behavior (`docs/architecture.md`) | Low, but cite carefully |
| Tests: `packages/core/tests/` (~82 files), backend/api/desktop test dirs, Playwright E2E both apps | Contract/golden coverage incl. `test_architecture_boundaries.py`, `test_orchestrator_contract.py`, `test_storage_injection.py` | confidence | **port the assertions, not the code** — boundary/contract/storage tests become Rust tests; E2E becomes GUI E2E (doc 14) | Med: coverage gaps hide in unported tests |

## Entry points (all)

- CLI: `wenyi=wenyi_cli.main:main` (`packages/cli/wenyi_cli/main.py`), app
  assembled in `cli.py`. **Spec only.**
- Web: `wenyi_api.main:app` via uvicorn (`apps/api/wenyi_api/main.py`) + arq
  workers (`queue.py`, `workers/`). **Removed.**
- Desktop Python: `wenyi_desktop/main.py` + `desktop.py` loopback server +
  `LocalRuntime`. **Removed after spec extraction.**
- Desktop native: `apps/desktop/src/main.rs` + Tauri sidecar
  (`scripts/desktop_sidecar.py`, PyInstaller). **Replaced by single binary.**

## Build / CI / packaging

- Python: uv workspace, `hatch-vcs` versions from git tags; lint `ruff
  check/format`, `pyrightconfig.json`; `scripts/` entry + PyInstaller engine.
  All **removed**; Rust toolchain + single-binary packaging replaces them
  (doc 10 §8).
- TS: pnpm workspaces, `openapi-typescript` schema generation, Playwright E2E
  per app. All **removed**.
- CI today: Python matrix + PG/Redis services + web E2E (`tests.yml`), CLI
  builds (`build.yml`), Tauri builds (`desktop.yml`). Replaced by Rust CI;
  Python/TS jobs die with their code.

## Dead code and duplication found

- **Web + desktop E2E duplication:** `apps/web/tests/` and
  `apps/desktop/frontend/tests/` overlap heavily (sidebar, shell, select,
  proofreading, style-editing specs); a single GUI E2E suite replaces both.
- **Generated schema:** `packages/shared-schema/src/api.d.ts` is build output
  masquerading as source — deleted with the TS stack, no port.
- **Design docs vs reality:** `docs/design/` holds proposals explicitly not
  describing implemented behavior (`docs/architecture.md`); never cite as
  spec — contradictions go to doc 16.
- **`pyproject.toml` dependency notes:** optional `weasyprint`/`fpdf2` PDF
  paths and AGPL-adjacent edges (BabelDOC) need explicit port decisions in
  doc 09 (formats) and doc 16 (D7); nothing ships accidentally.
- **Dual backends, dual hosts, dual E2E:** the web/desktop platform split
  doubles every platform-adjacent cost — the monolith exists to end this.

## Critical migration path

```text
pure core first (markup, postprocess, segmenter, glossary matcher)
 → storage (SQLite schema + artifact keys + atomic commits)
 → pipeline stages (prepare → translate → polish → review/autofix → finalize)
 → LLM router + providers (against FakeClient goldens, then live spot-checks)
 → formats (EPUB round-trip first, then DOCX/HTML/text/SRT, PDF last)
 → CLI-spec headless runner (proves engine without GUI)
 → native GUI screens (dashboard → progress → proofreading/review → rest)
 → OS integration (instance/tray/dialogs/updates/keyring)
 → parity gate → delete Python/TS/web/docker/deploy → single binary release
```

Rationale: each step is testable without the next; the engine is proven before
the GUI exists; deletion happens once, at the end (details: docs 04 and 15).

## Sources

- `AGENTS.md` (boundaries, invariants, entry points, test commands)
- `docs/architecture.md` + `docs/design/component-decoupling.md` (map vs proposal)
- `packages/core/wenyi_core/` (all subdirs listed in doc 02)
- `packages/core/tests/` (82 files incl. boundary/contract/storage tests)
- `apps/api/wenyi_api/db/schema.sql`, `queue.py`, `main.py`
- `apps/desktop/backend/wenyi_desktop/` (all modules), `apps/desktop/src/main.rs`
- `packages/shared-schema/src/api.d.ts` (generated)
- `pyproject.toml`, `.github/workflows/` (3 files), `scripts/`, `docker/`, `deploy/`
- `wc -l` measurements from doc 02

## Open questions

See `16-decisions-and-open-questions.md`.
