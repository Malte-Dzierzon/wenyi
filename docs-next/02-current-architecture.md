# Current Architecture (Audited)

Verified against the repository as of 2026-10-25. Paths below are repo-relative.
LOC figures are `wc -l` sums over `.py`/`.ts`/`.tsx`/`.rs` files (including
tests/E2E unless noted); they measure scale, not importance.

## Component diagram

```mermaid
flowchart TB
    subgraph CLI["CLI (local, file state)"]
        WCLI[wenyi_cli<br/>Typer commands]
    end
    subgraph WEB["Web (self-hosted, multi-user)"]
        WAPP[apps/web<br/>React/Vite shell]
        API[apps/api/wenyi_api<br/>PG + Redis/arq adapters]
    end
    subgraph DESK["Desktop (single-user, local)"]
        TSHELL[apps/desktop/src<br/>Tauri 2 host: 9 Rust modules]
        DFE[apps/desktop/frontend<br/>React shell]
        DPY[apps/desktop/backend/wenyi_desktop<br/>SQLite + local runner]
    end
    subgraph SHARED["Shared Python"]
        CORE[packages/core/wenyi_core<br/>engine]
        BE[packages/backend/wenyi_backend<br/>HTTP contract + app services]
        UI[packages/ui<br/>shared React pages]
        SCHEMA[packages/shared-schema<br/>generated api.d.ts]
    end
    WCLI --> CORE
    WAPP --> API
    DFE --> DPY
    TSHELL -. spawns/monitors .-> DPY
    API --> BE --> CORE
    DPY --> BE --> CORE
    WAPP --> UI
    DFE --> UI
    UI --> SCHEMA
    API -. Postgres/Redis/arq .-> PG[(PostgreSQL + Redis)]
    DPY -. SQLite catalog + per-project stores .-> LITE[(SQLite)]
    WCLI -. state dirs + manifest.json .-> FILES[(files)]
```

## Per-subsystem inventory

| Subsystem | Role / entry | Key files / symbols | Scale |
|---|---|---|---|
| `packages/core/wenyi_core` | Translation engine; no CLI/HTTP deps (`AGENTS.md` boundary) | `pipeline/orchestrator.py` + `Orchestrator` facade; `pipeline/preparation.py` `PreparationService`; `pipeline/translation.py`, `translation_batch.py`, `context.py`, `title_translation.py`; `pipeline/review_workflow.py`, `review_checkpoint.py`, `review_rounds.py`, `review_chunks.py`, `review_results.py`; `pipeline/review_autofix.py`, `autofix_candidates.py`, `autofix_plan.py`, `autofix_publish.py`, `autofix_verification.py`; `pipeline/finalization.py`; `pipeline/runstore.py` `RunStore`; `agents/` (translator, polisher, reviewer, review_loop, review_fixer, review_arbiter, analyzer, synopsis, title_translator, annotation_aligner, precision); `ingest/` readers + `models.py` Segment/Chapter/Document + `tokens.py`; `assemble/` writers; `llm/` router + `providers/`; `glossary/store.py` `GlossaryStore` + extractor + resolver; `i18n/` + `i18n/data/` prompt/registry resources; `markup/` anchors/annotations/ruby; `review/evidence.py` `BookEvidenceIndex`; `srt/`; `storage/protocol.py` `Storage`/`ArtifactStorage`, `file.py`, `sqlite.py`; `config.py` `_DEFAULT_CONFIG_YAML` | ~29.4k |
| `packages/cli/wenyi_cli` | Local CLI; direct core import, no HTTP | `cli.py` assembles app, `main.py` entry (`wenyi=wenyi_cli.main:main`); `commands/` (bootstrap, workflows, inspection, progress, glossary, validation, presentation, context) + `model_commands.py` | ~1.4k |
| `packages/backend/wenyi_backend` | Shared HTTP contract + app services; no PG/Redis/arq/keyring imports | `application.py` `create_app` + Bearer `TokenMiddleware`; `context.py` `BackendContext`; `routers/` (health, strategies, projects, configuration, settings, report, subtitles, chapters, glossary, review, style, export, events, ws); `workers/`; `emitters.py` | ~4.0k (package only) |
| `apps/api/wenyi_api` | Web platform adapters: PG pool, Redis, arq | `main.py` (`wenyi_api.main:app`, uvicorn); `adapters.py` `PostgresRepository`/Settings/Exports, `RedisTelemetry`, arq enqueue (`wenyi:workflows`, `wenyi:exports`); `storage_pg.py` `PostgresStorage`; `db/schema.sql` 13 tables; `queue.py`, `workers/` | ~2.3k (package) / ~6.0k incl. tests |
| `apps/desktop/backend/wenyi_desktop` | Local platform: SQLite catalog, in-process runner | `desktop.py` (loopback `:0`, `WENYI_READY`, `watch_parent`), `main.py`; `local_backend.py`, `local_runtime.py` (asyncio queues + progress hub), `catalog_recovery.py`, `local_credentials.py` (keyring), `local_export_files.py`; `routers/` incl. desktop-only credentials/desktop_export/external_credentials/updates | ~2.8k (package) / ~7.3k incl. tests |
| `apps/desktop/src` | Tauri 2 host: OS integration only | `main.rs` (7 `tauri::generate_handler` commands: `native_drop_*`, `native_export_save`, `desktop_update_*`), `process.rs`, `instance.rs` (single-instance), `background.rs`, `updates.rs`, `native_drop.rs`, `native_export.rs`, `local_connection.rs`, `graphics.rs`; tray, sidecar spawn, rfd dialogs, signed updater | ~5.2k Rust |
| `apps/desktop/frontend/src` | Thin desktop React host | `main.tsx`, `platform.ts`, `transport.ts`, `runtime.ts`, `nativeDrop.ts`, `nativeExport.ts`, `mineruCredentials.ts`, `updateSafety.ts` | ~1.2k |
| `apps/web/src` | Thin web React host | `vite.config.ts`, tests; renders shared UI | ~0.1k src (~7.8k with Playwright E2E) |
| `packages/ui/src` | All shared pages/components | `App.tsx`; `routes/manifest.tsx` 13 routes; `features/` (dashboard, project-create, progress, glossary, style, contents, review, proofreading, export, events, settings, subtitles); `lib/api.ts` ~80 methods, `lib/ws.ts` reconnect/backoff, `platform.ts`; `i18n/` en + zh-CN | ~11.5–14k |
| `packages/shared-schema/src` | Generated TS types | `api.d.ts` via openapi-typescript from running API | ~3.8k (generated) |
| `scripts/` | Entry/packaging helpers | `desktop_entry/`, `desktop_sidecar.py` (PyInstaller engine), `desktop.mjs`, `package_release/`, `check_*` | ~1.0k |
| `docker/` + `deploy/` | Web images + compose (pg16/redis7/api/worker/export-worker/web) + nginx | Dockerfiles, compose | config only |
| `.github/workflows/` | CI | `tests.yml` (py matrix + PG/Redis services + web E2E), `build.yml` (CLI PyInstaller), `desktop.yml` (Tauri) | config only |
| `docs/` + `docs/zh/` + `docs/design/` | Current-code docs + proposals | `pipeline.md`, `architecture.md`, `web.md`, `desktop.md`, `cli.md`, `configuration.md`; `design/` proposals (not implemented) | docs only |

## Dependency map

Enforced direction (`AGENTS.md`):

```text
CLI / Web / Desktop → Orchestrator → Runtime / Preparation / Translation /
                      Review / ReviewAutofix / Finalization
                   → agents / ingest / glossary / assemble / Storage
```

- `wenyi_core` ← `wenyi_backend` ← `wenyi_api` / `wenyi_desktop`; CLI imports
  core directly. Verified: backend owns `BackendContext` ports
  (`packages/backend/wenyi_backend/context.py`); workers call `Orchestrator`
  directly; `test_architecture_boundaries.py` guards the direction.
- UI hosts (`apps/web`, `apps/desktop/frontend`) → `@wenyi/ui` →
  `packages/shared-schema` (`api.d.ts`); interface changes must sync backend
  schema → shared types → frontend calls (`AGENTS.md`).
- `pdf_bridge/` is HTTP-only (BabelDOC AGPL isolation); main tree sends HTTP
  only, no AGPL Python dependency (`AGENTS.md`).

## Why web + desktop coexist

Per `docs/architecture.md`: "Web and Desktop share behavior, not project state
or infrastructure." Two platforms serve different users over one shared HTTP
contract (`wenyi_backend`):

- **Web** = self-hosted multi-user workspace: PostgreSQL persistence +
  advisory locks, Redis/arq job queues, `DATA_DIR` for uploads/exports.
- **Desktop** = single-user local app: isolated workspace, SQLite catalog
  (`local.sqlite3`) + per-project `SqliteStorage`, in-process asyncio runner,
  OS keyring for secrets.

## Desktop-is-web-embedded finding

The "desktop app" is a web stack in a native frame:

- **Rust is host/OS layer only** (`apps/desktop/src`): process management,
  single-instance admission, tray, updater, file dialogs, drag-drop grants —
  no domain logic.
- **Python is the domain** (`wenyi_desktop` + sidecar engine): HTTP API,
  project state, pipeline execution.
- **React is the UI** (`packages/ui` + thin frontend host): all 13 screens.

The rewrite therefore replaces all three layers (doc 05), salvaging only
behavioral semantics and OS-integration patterns (doc 10).

## Storage split per platform

| Platform | State location | Manifest/commit | Locks |
|---|---|---|---|
| CLI | `state/<slug>/targets/<lang>/` files | `manifest.json` committed last, atomic via `os.replace` | book-level run lock |
| Web | PostgreSQL per project; `DATA_DIR` uploads/cache/exports only | transactions + advisory locks | advisory locks |
| Desktop | isolated workspace; catalog SQLite + per-project `SqliteStorage` | atomic temp-file + publish; native export picks target before enqueue | run/state/event locks, no long write txns |

Invariants across all three (`AGENTS.md`): `target None` = pending (`""`
valid for MinerU PDFs); review writes shadow-only, autofix publishes via
recoverable index; usage append-once; stable `Segment` ids; keys env/keyring
only, never plaintext.

## Transport contracts

- **REST** via `wenyi_backend` routers + **Bearer** `TokenMiddleware`
  (`packages/backend/wenyi_backend/application.py`); desktop adds loopback
  `:0` + `WENYI_READY` protocol (`wenyi_desktop/desktop.py` + `watch_parent`).
- **WS** `/ws/projects/{pid}/progress`
  (`packages/backend/wenyi_backend/routers/ws.py:16`): `TranslationEvent`
  (`packages/core/wenyi_core/events.py` + `kind`, `done/total/label/payload`).
  Core kinds: `progress | batch | chapter | term | pipeline | log`
  (`events.py:30`); transport/state kinds observed in code: `snapshot`,
  `state`, `stats` (`emitters.py`, grep-verified `kind=` occurrences).
  `packages/ui/src/lib/ws.ts` handles reconnect/backoff.

## Sources

- `AGENTS.md` (boundaries, state invariants, entry points)
- `docs/architecture.md` (module map, web/desktop split)
- `README.md` (functionality, providers, formats)
- `packages/core/wenyi_core/events.py:30` (event kinds)
- `packages/backend/wenyi_backend/application.py:38-49` (Bearer middleware)
- `packages/backend/wenyi_backend/routers/ws.py:16` (WS route)
- `apps/desktop/backend/wenyi_desktop/desktop.py:61-101` (loopback/ready/parent-watch)
- `apps/desktop/src/main.rs` (Tauri commands)
- `packages/ui/src/routes/manifest.tsx` (route inventory)
- `apps/api/wenyi_api/` (`main.py`, `storage_pg.py`, `queue.py`, `db/schema.sql`)
- `wc -l` runs over repo trees (method noted above)

## Open questions

See `16-decisions-and-open-questions.md`.
