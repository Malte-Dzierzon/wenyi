# Data and Persistence

Current state per platform, the unified `catalog.sqlite` + per-project `project.sqlite` target, schema, and migration story.

## 1. Current state

### 1.1 CLI: files + glossary.db

`state/<slug>/targets/<target>/` (`translation_run_dir`, `pipeline/runstore.py:42-45`): `manifest.json` (book metadata + chapter status, committed last as the initialization authority), `chapters/ch{n}.json`, `context.json`, `analysis.json`, `annotation_contexts.json`, `report.json`, `usage.json`, `events.jsonl` (append-only), `glossary.db` (SQLite terms/conflicts), `reviews/` (shadow review runs + publication indices), `source/` (parser caches). Atomic writes via same-directory temp file + `os.replace` (`runstore.py:170-178`). Four OS file locks (`runstore.py:63-106`): `.run.lock` (long workflow), `.state.lock` (short consistent snapshots, reentrant per thread), `.events.lock`, `.assemble.lock` (+`.timing.lock`). `target is None` = pending; MinerU `""` is a real value (AGENTS.md).

### 1.2 Web: PostgreSQL (13 tables) + Redis + advisory locks

`apps/api/wenyi_api/db/schema.sql` (no migrations — fresh deployments only): `application_settings`, `projects` (config/manifest/context/analysis/usage/report JSONB, `source_sha256`), `chapters`, `segments`, `segment_revisions`, `glossary` (+`term_conflicts`, trigram indexes), `artifacts` + `artifact_events`, `events`, `exports`, `jobs`, `strategy_templates`. `PostgresStorage` (`apps/api/wenyi_api/storage_pg.py:3-5`): mutable state in PG, `run_dir` holds only source resources/parser caches/exports; long workflow locks are session advisory locks (`pg_advisory_lock`), state writes are short xact-locked transactions with no model calls inside. Export retention uses `pg_try_advisory_lock` (`export_retention.py:32-44`); `jobs` rows (kind/status/arq_job_id/run_id/params) feed the arq recovery loop (`workers/recovery.py:28-35`, 2-minute grace). Redis roles: `RedisEmitter` progress pub/sub + TTL caches (`packages/backend/wenyi_backend/emitters.py:14-41`) and `StatisticsCache` live snapshots (`live_statistics.py:87-107`).

### 1.3 Desktop: catalog + per-project SqliteStorage

`LocalBackend` (`apps/desktop/backend/wenyi_desktop/local_backend.py:40-70`): `local.sqlite3` with `projects(id, document)`, `jobs(id, project_id, run_id, arq_job_id, document)`, `exports(...)`, `application_settings(id=1, document)`, `progress(id, document)`; WAL mode, transaction-scoped connections, no app-wide pool (`local_backend.py:73-99`). Per-project state in `SqliteStorage` (`packages/core/wenyi_core/storage/sqlite.py:34-40`): one target-isolated `state.sqlite3` (WAL, `busy_timeout=5000`, `busy_timeout` + reentrant mutex, savepoint-nested state locks), schema = `artifacts(key, value)` + `artifact_records(sequence, key, value)` + `chapter_statistics` + `segment_revisions` + glossary tables (`sqlite.py:85-120`). `LocalRuntime` (`local_runtime.py:194-386`): bounded in-process task slots + `ProgressHub` cache; cancellation waits for the synchronous checkpoint boundary.

### 1.4 Invariants carried forward

Artifact keys are run-dir-relative `/`-separated names, prefix-scanned with bounded scope; `source_sha256` binds content identity; usage/events are append-once ledgers (review increments merged once — `llm/usage.py:198-203` `merge_usage_summaries`); review writes shadow copies, autofix publishes via recoverable index (AGENTS.md state section).

## 2. Target: unified workspace layout

```text
<workspace>/
├── catalog.sqlite                 # projects, jobs, exports, settings (WAL)
└── projects/<id>/                 # <id> = uuid7 hex (sortable, collision-free)
    ├── project.sqlite             # ALL mutable state (WAL)
    ├── source/                    # uploaded original + parser caches (opaque bytes)
    └── exports/                   # rendered output files (opaque bytes)
```

`catalog.sqlite` (global + scheduling; never translation truth):

```sql
CREATE TABLE projects(
  id TEXT PRIMARY KEY, name TEXT NOT NULL, fmt TEXT NOT NULL,
  source_lang TEXT, target_lang TEXT NOT NULL,
  source_sha256 TEXT CHECK(source_sha256 IS NULL OR source_sha256 GLOB '[0-9a-f]*'),
  source_path TEXT,                 -- original filename only; bytes live under projects/<id>/source/
  status TEXT NOT NULL DEFAULT 'created', error TEXT,
  config TEXT NOT NULL DEFAULT '{}',          -- frozen creation snapshot (expanded preset + policy fingerprint)
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE jobs(
  id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,               -- translation | chapter_translation | review | srt | export
  status TEXT NOT NULL DEFAULT 'queued',      -- queued | running | paused | done | error | cancelled
  run_id TEXT NOT NULL UNIQUE, params TEXT NOT NULL DEFAULT '{}',
  result TEXT, error TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
-- NOTE: deliberately drops today's `arq_job_id` (Redis/arq identity has no target equivalent); `run_id` (uuid7) is the sole identity.
CREATE INDEX idx_jobs_project ON jobs(project_id, id DESC);
CREATE TABLE exports(
  id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  format TEXT NOT NULL, options TEXT NOT NULL DEFAULT '{}', path TEXT, size INTEGER,
  status TEXT NOT NULL DEFAULT 'pending', error TEXT,
  created_at TEXT NOT NULL, completed_at TEXT);
CREATE TABLE settings(id INTEGER PRIMARY KEY CHECK(id=1), document TEXT NOT NULL,
  default_template TEXT NOT NULL, revision INTEGER NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE provider_refs(provider TEXT PRIMARY KEY, keyring_account TEXT NOT NULL);
-- no secret material; see 11-configuration.md §3.2
```

`project.sqlite` (per project; replaces manifest/chapters JSON + glossary.db + reviews/ + usage.json + events.jsonl):

```sql
PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;
CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);  -- manifest, context, analysis, report, usage, policy fingerprint
CREATE TABLE chapters(seq INTEGER PRIMARY KEY, title TEXT NOT NULL DEFAULT '',
  title_translated TEXT, href TEXT, status TEXT NOT NULL DEFAULT 'pending',       -- pending | done
  review_status TEXT NOT NULL DEFAULT 'pending', meta TEXT NOT NULL DEFAULT '{}');
CREATE TABLE segments(project implied, chapter_seq INTEGER NOT NULL REFERENCES chapters(seq) ON DELETE CASCADE,
  seg_seq INTEGER NOT NULL, source TEXT NOT NULL DEFAULT '', target TEXT,         -- NULL = pending; '' = real empty (MinerU)
  target_before_polish TEXT, kind TEXT NOT NULL DEFAULT 'text', anchor TEXT,
  cont INTEGER NOT NULL DEFAULT 0, meta TEXT NOT NULL DEFAULT '{}',
  PRIMARY KEY (chapter_seq, seg_seq));
CREATE TABLE segment_revisions(id INTEGER PRIMARY KEY AUTOINCREMENT,
  chapter_seq INTEGER NOT NULL, seg_seq INTEGER NOT NULL, kind TEXT NOT NULL,
  previous_target TEXT, new_target TEXT, created_at TEXT NOT NULL);
CREATE TABLE glossary(source TEXT PRIMARY KEY, target TEXT NOT NULL, reading TEXT NOT NULL DEFAULT '',
  type TEXT NOT NULL DEFAULT 'term', gender TEXT NOT NULL DEFAULT '',
  aliases TEXT NOT NULL DEFAULT '[]', first_chapter INTEGER, note TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'ok', updated_at REAL);
CREATE TABLE term_conflicts(id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL,
  existing_target TEXT, proposed_target TEXT, chapter INTEGER, note TEXT,
  resolved INTEGER NOT NULL DEFAULT 0, created_at REAL);
CREATE TABLE artifacts(key TEXT PRIMARY KEY, value TEXT NOT NULL);               -- reviews/*, timing, snapshots
CREATE TABLE artifact_records(sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  key TEXT NOT NULL REFERENCES artifacts(key) ON DELETE CASCADE, value TEXT NOT NULL);
CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT,
  type TEXT NOT NULL, payload TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL);
CREATE INDEX idx_events_type ON events(type, id DESC);
```

Why one SQLite file per project (+WAL) instead of JSON: single-writer transactions replace four OS lock files (SQLite busy-timeout + a small lock table is enough for one desktop user); prefix scans become indexed queries; 10k-page books stay lazy (chapter/segment pagination instead of whole-file JSON parses). Why separate `catalog.sqlite`: listing 500 projects must not open 500 databases.

## 3. Locking and atomicity (SQLite equivalents)

| Old | New |
| --- | --- |
| `.run.lock` (long workflow) | `jobs` row `running` + in-process `EngineHandle` ownership; stale `running` rows (heartbeat older than N) are recoverable, never auto-deleted |
| `.state.lock` | Short `BEGIN IMMEDIATE` write transactions; reads use snapshot queries, no lock held across awaits/model calls |
| `.events.lock` / append-only JSONL | `events` table INSERT (SQLite serializes writers); `artifact_records` for review-round ledgers |
| temp-file + `os.replace` | SQLite transaction commit (ACID); `exports/` file publish keeps temp-file + rename for the final user-visible file only |
| glossary WAL-snapshot reads | Same: read-only query against WAL snapshot, no write tx held during the run |

## 4. Secrets

`keyring` crate, service `org.wenyi.desktop` (as today). `provider_refs` maps provider name → keyring account; secrets never touch `wenyi.yaml`, either SQLite file, logs, or diagnostics bundles. Resolution: keyring → env (`*_API_KEY`) → actionable error; memory-only session fallback with explicit warning.

## 5. Migration story

Fresh-import-first: v1 ships with empty-workspace creation + source-file import (parse → new `project.sqlite`). Per D6 (doc 16) there is no legacy importer at v1 (`state/` trees, `config.yaml` files, PG data stay behind). Schema versioning from day one: `PRAGMA user_version` on both files; forward-only migrations. No PG/Redis import path — web data stays on web.

## Sources

- `packages/core/wenyi_core/pipeline/runstore.py:1-10` (layout docstring), `:42-56` (run dir), `:63-107` (locks), `:155-178` (chapter paths, atomic write), `:186-238` (initialization protocol)
- `packages/core/wenyi_core/storage/protocol.py` (Storage/ArtifactStorage ports, key conventions)
- `packages/core/wenyi_core/storage/sqlite.py:34-125` (SqliteStorage, WAL, schema), `storage/locks.py` (flock/msvcrt)
- `apps/api/wenyi_api/db/schema.sql` (all 13 tables + trigram indexes)
- `apps/api/wenyi_api/storage_pg.py:3-109`, `dal.py:217-277`, `export_retention.py:17-44`, `workers/recovery.py:28-90`
- `apps/desktop/backend/wenyi_desktop/local_backend.py:40-99`, `local_runtime.py:43-196`
- `packages/core/wenyi_core/llm/usage.py:198-203` (append-once merge), `packages/core/wenyi_core/events.py` (TranslationEvent kinds)

## Open questions

See `16-decisions-and-open-questions.md` (heartbeat interval + stale-job UX; legacy importer priority; export-retention limit parity — web keeps 5 per project via `EXPORT_LIMIT`).
