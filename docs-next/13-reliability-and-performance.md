# Reliability and Performance

Resume/checkpoint/cancel/pause/recovery today, gaps for 10k-page books, and the target persistent job queue, crash-recovery protocol, perf budgets, and diagnostics.

## 1. Current resume / checkpoint / cancel / pause / recovery

- CLI resume: completed batches/chapters are skipped on re-run (`resume_batches`, `pipeline/translation_batch.py:117-120`); usage persists cumulatively in `usage.json` and review increments merge once (`llm/usage.py:198-203`); a saved `manifest.json` is the init authority and `begin_initialization` wipes derived state on re-init (`runstore.py:186-238`).
- Cancel: `LocalRuntime._cancel` cancels the task but the worker only stops at the synchronous checkpoint boundary (`local_runtime.py:253-257`); native export plans cancel before dialog completion creates no job (`native_export.rs` quit-can-cancel test).
- Pause: web sets project `paused` on interruption (`workers/recovery.py:81-87`); desktop `_interrupt` marks jobs on shutdown enqueue path (`local_runtime.py:270`).
- Crash recovery, web: 30 s `_recovery_loop` (`workers/__init__.py:23-31`) + `recover_jobs` — 2-minute grace, then non-blocking lock probe decides worker death vs. live worker, `queued` rows re-checked against arq (`JobStatus`, `recovery.py:28-90`); interrupted workflows → `interrupted` + resumable, exports → `error` + recreate.
- Crash recovery, desktop: `WorkspaceLock` (one task-owning process per workspace, survives crashes, `local_runtime.py:43-87`) + `recover(startup=True)` on `LocalRuntime.start` (`local_runtime.py:218-232`) + periodic `_recover_periodically`.
- Export safety: temp-file + atomic publish; open native dialogs and active transfers block updates (`native_export.rs:155-158` tests).
- Secrets never in logs/diagnostics: keys env-only or keyring, never plaintext in SQLite/YAML/logs (AGENTS.md; `local_credentials.py:41-67`).

## 2. Gaps for 10k-page books

1. Whole-file JSON chapter state: every chapter save/load parses the full chapter; a 10k-page book means 10k JSON files each re-read per review/export pass. Needs paged segment access (target schema §3 in doc 12).
2. Event/progress fan-out per segment: current kinds (`progress/batch/chapter/term/pipeline/log/stats/final`, `ws.ts:5-17`) at segment granularity would flood the GUI; needs throttled coarse progress + targeted invalidation.
3. Memory: full-book `Document`/chapter graphs held during translation and review; needs streaming chapter-at-a-time processing with bounded rolling context (6 segments today — `config.yaml:29`).
4. Retries vs. budgets: transport retries are centralized (`llm/retrying.py`, provider SDK retries must stay off per AGENTS.md) but a 10k-page run multiplies cost of unbounded `review_fix_max_rounds`/evidence rounds; per-run `BudgetConfig(max_requests/max_tokens/deadline_seconds)` (`llm/configuration.py:89-95`) must be enforced pre-flight with a cost estimate, not just monitored.
5. Duplicate work: batch re-packing after config change can mix saved and pending targets (`resume_batches`); the target must key idempotency on (chapter, segment, policy fingerprint), not file position.
6. Updater + long runs: updates blocked only during dialogs/transfers today; a 24 h translation needs update deferral until job idle.

## 3. Target: persistent job queue in SQLite

One `jobs` table in `catalog.sqlite` (schema in doc 12) replaces arq + Redis queues + in-memory asyncio queues. Columns `kind/status/run_id/params/result/error` mirror today's `dal.create_job` contract (`dal.py:217-234`) minus `arq_job_id` — the `run_id` (uuid7) is the sole identity. Engine owns a bounded worker pool (default: 1 workflow slot + 2 export slots, matching `LocalRuntime` consumer counts at `local_runtime.py:223-228`); queue depth is unbounded in SQLite, admission is in-process. Cancel = mark `cancelled` + worker stops at the next checkpoint boundary (same semantics as today); pause = mark `paused` + checkpoint; resume replays from the persisted per-chapter/per-segment state, skipping `done` units. Heartbeat: running rows carry `updated_at` touched every 30 s; startup recovery treats rows with stale heartbeats as interrupted (same 2-minute grace as `recover_jobs`), live rows are left alone.

## 4. Crash-recovery protocol

1. On startup, acquire workspace lock (single owner; fail closed with actionable message, porting `WorkspaceLock`/`instance.rs` behavior).
2. Scan `jobs WHERE status IN ('queued','running')`; rows older than the grace period with no live owner → `interrupted` (+ `task_interrupted` event row) or export rows → `error` (+ `export_interrupted`).
3. Resume is explicit: the user presses Resume on an `interrupted` job; the engine replays from persisted chapter/segment state. Never auto-resume model-spending work after a crash.
4. `usage` ledger recovery: `recover_usage` semantics (drop uncommitted increments, keep committed totals) preserved so retries never double-bill.
5. Export rows: never auto-retry; user creates a new export (as today).

## 5. Progress events (in-process)

Same taxonomy as `TranslationEvent` (`events.py:21-34`) and `ws.ts` kinds, delivered over an mpsc/broadcast channel to an Iced `Subscription` (doc 10 §6): `snapshot | state | progress | batch | chapter | term | pipeline | log | stats | final`. Throttle: coarse `progress` at ≤4 Hz; `chapter`/`batch` on completion only; `stats` (usage/timing) on change. Chapter lists render from paged SQLite reads; event handlers invalidate rows, never rebuild lists.

## 6. Perf budgets

| Operation | Budget (p50 desktop, Linux) |
| --- | --- |
| Cold startup (window shown, catalog of 500 projects listed) | ≤ 1.5 s; project DBs opened lazily |
| Chapter list for 10k-chapter book (paged, 100/chapter page) | ≤ 200 ms per page |
| Segment edit → persisted + visible | ≤ 100 ms (single short write tx) |
| Progress frame handling during full-speed translation | ≤ 4 Hz coarse updates; no dropped engine throughput |
| Memory ceiling, idle with large project open | ≤ 250 MB (no full-book graphs in memory) |
| Memory ceiling, translating | ≤ 1 GB (chapter-at-a-time + bounded context) |

## 7. Diagnostics and logging

`tracing` with per-module spans (`engine::translate`, `engine::review`, `storage`, `net::llm`); log levels default WARN, `WENYI_LOG` overrides. Problem-report bundle (from Settings → Export diagnostics): redacted `wenyi.yaml` (no secrets), `catalog.sqlite` job rows (no keyring material), recent log tail, engine + OS versions, policy fingerprints — explicitly excluding `project.sqlite` segment text and any secret. Report format mirrors today's `report.json`/`timing.json` content, minus book text.

## Sources

- `packages/core/wenyi_core/pipeline/runstore.py:186-238` (init/resume), `translation_batch.py:117-120` (resume_batches)
- `packages/core/wenyi_core/llm/usage.py:198-244` (append-once merge, UsageTracker), `events.py:21-63`
- `apps/api/wenyi_api/workers/recovery.py` (full), `workers/__init__.py:29-51` (recovery loop)
- `apps/desktop/backend/wenyi_desktop/local_runtime.py:43-87` (WorkspaceLock), `:194-292` (cancel/checkpoint/queues), `:389-404` (snapshots)
- `apps/desktop/src/native_export.rs:154-158,428-446,906-1101` (atomic publish, update gating, tests)
- `packages/ui/src/lib/ws.ts:5-83` (event kinds, 500 ms coalescing)
- `apps/api/wenyi_api/storage_pg.py:86-109` (session advisory locks), `export_retention.py` (EXPORT_LIMIT=5)
- `packages/core/wenyi_core/llm/configuration.py:89-95` (BudgetConfig)

## Open questions

See `16-decisions-and-open-questions.md` (heartbeat/grace-period constants; cost-estimate UX before long runs; diagnostics-bundle retention).
