# Rust migration — port-vs-redesign and interop policy

How the Python engine becomes the Rust monolith from 05. For what is ported,
see 04; for where it lands, see 05.

## 1. Method

Behavioral parity via fixtures, not co-execution. No Python embedding, no FFI,
no dual-runtime strangler: embedding CPython would drag the GIL, the 5-member
uv workspace, and AGPL-adjacent PDF tooling into the single binary and violate
the process-isolation rule (`AGENTS.md`). Instead:

1. Freeze golden fixtures from the Python implementation (manifest/chapter
   JSON, glossary DB dumps, prompt renderings, event sequences) using
   `FakeClient` (`llm/providers/fake.py:37`) runs over a small sample corpus
   covering every input format and both translation modes.
2. Port module-by-module in the seam order of §5, each slice green against its
   fixtures before the next begins.
3. Delete the Python slice only at the end (big-bang cutover per crate, §5);
   CI runs fixture comparison until then.

`FakeClient` exists precisely for offline deterministic runs (`AGENTS.md`
validation section) — it becomes the parity harness rather than throwaway test
code.

## 2. Port-vs-redesign rules

- **Ports cleanly** (pure logic, no runtime coupling): pure functions over
  plain data — punctuation (`postprocess/`), prompt renderers
  (`agents/prompts.py`), glossary matcher math (`glossary/store.py:102-216`),
  evidence queries (`review/evidence.py`), review session transitions
  (`review/session.py`), i18n language/policy tables (`i18n/`), prompt
  templates (`i18n/data/tasks/`), config schema (`config.py`, `llm/configuration.py`).
- **Needs redesign** (runtime, mutation, or framework coupling): async fan-out
  (threading → tokio tasks + broadcast), `RunStore` file-per-chapter JSON →
  SQLite rows with equivalent lock scopes, manifest-last atomicity → SQLite
  commit atomicity, `os.replace` temp-file dance → transaction + export
  snapshot reads, Pydantic validation → serde + explicit validators, tenacity
  decorators → `provider_retry` equivalent with backoff in `wenyi-llm`,
  Typer app → clap derive tree, FastAPI routers → in-process GUI operations,
  arq/PG/WebSocket → in-process queues and event bus.

## 3. Module mapping (Python → Rust crates)

Covers every `wenyi_core` subpackage plus the app-layer sources. Target crates
from 05: `store`, `llm`, `doc`, `engine`, `gui`, `cli`, `app`.

| Python source | Rust target | Treatment |
|---|---|---|
| `pipeline/orchestrator.py:30` | `engine::orchestrator` | Redesign: same facade surface (`prepare/run/review/report/assemble`), tokio-backed, lock scopes via `store` |
| `pipeline/preparation.py:70` | `engine::preparation` | Port stages; redesign I/O onto `store` traits |
| `pipeline/translation.py:43` + `translation_batch.py`, `context.py`, `title_translation.py` | `engine::translation` | Port algorithms (alignment, batch glossary, precision branch `translation.py:327`); redesign concurrency |
| `pipeline/review_workflow.py:37`, `review_checkpoint.py`, `review_chunks.py`, `review_results.py`, `review_rounds.py` | `engine::review` | Port session math; redesign orchestration onto event bus |
| `pipeline/review_autofix.py` + `autofix_*.py` | `engine::autofix` | Port exactly incl. recoverable-index-first invariant; type-level publish capability |
| `pipeline/finalization.py:27,59` | `engine::finalize` | Port report/assemble logic; snapshots become DB transactions |
| `pipeline/runstore.py:48`, `runtime.py`, `input_preparation.py`, `language_policies.py`, `precision*.py`, `annotations.py`, `docx_styles.py` | `store` (paths/locks) + `engine` (services) | Split: state mechanics → `store`, decisions → `engine` |
| `agents/base.py:24` | `engine::agent` trait | Redesign as trait (`complete` + phase policy) |
| `agents/translator.py`, `title_translator.py`, `analyzer.py`, `synopsis.py`, `polisher.py`, `precision.py`, `annotation_aligner.py` | `engine::agents::*` | Port prompts + parsing; prompts embedded from `i18n/data/tasks/` |
| `agents/reviewer.py`, `review_loop.py`, `review_actions.py`, `review_fixer.py`, `review_arbiter.py` | `engine::review_agents::*` | Port evidence/fix protocols exactly |
| `agents/prompts.py` | `doc::prompts` | Port as pure functions |
| `ingest/` (models, segmenter, tokens, source_hash, all readers, `pdf_to_html.py`, `pdf_babeldoc.py`) | `doc::ingest::*` | Port algorithms; `zip`/`quick-xml`/`scraper` replace ebooklib/lxml/bs4; MinerU/BabelDOC stay HTTP via `llm` transport |
| `assemble/` (all writers, navigation/presentation/resources, `export_view.py`, `policy.py`, `report.py`, `about.py`) | `doc::assemble::*` | Port semantics; writers rebuilt on `zip`/`quick-xml` (no `epub`/`docx-rs` — see 05 §6) |
| `markup/` (anchors, annotations, segments, ruby, contracts) | `doc::markup` | Port cleanly (pure) |
| `document_styles/docx.py` | `doc::docx_styles` | Port style policy exactly |
| `postprocess/` | `doc::postprocess` | Port first (pure, test-pinned) |
| `glossary/store.py`, `extractor.py`, `resolver.py` | `doc::glossary` | Port schema + matcher exactly; SQLite via `store` connection |
| `review/models.py`, `contracts.py`, `evidence.py`, `session.py`, `run_store.py`, `conflicts.py`, `autofix_models.py` | `doc::review` + `engine::review` | Split: data/evidence/session → `doc`; drivers/publish → `engine` |
| `srt/policy.py`, `store.py`, `translate.py` | `engine::srt` (+ `doc::srt_format`) | Port as-is, keep Orchestrator-independent |
| `pdf_bridge/client.py:17` | `llm::bridge` (transport) | Port HTTP calls; keep process isolation (external service or sidecar, never linked AGPL code) |
| `llm/base.py`, `factory.py`, `router.py`/`routing.py`, `operations.py`, `registry.py:113-139` | `llm::client/router/operations/registry` | Port protocol knowledge; redesign as reqwest control loops |
| `llm/providers/` (10 kinds) | `llm::providers::*` | Port per-provider auth/fields/quirks; provider-owned extras stay provider-local |
| `llm/retrying.py`, `transport.py`, `limits.py`, `usage.py`, `connectivity.py`, `migration.py`, `models.py`, `credentials.py`, `errors.py`, `json_parser.py` | `llm::*` | Port policies (retry/backoff, budgets, redaction); redesign I/O |
| `storage/protocol.py`, `file.py`, `sqlite.py`, `artifacts.py`, `locks.py`, `language_policies.py`, `precision_archive.py`, `precision_drafts.py` | `store::*` | Redesign on rusqlite; keep trait seams and atomicity/lock semantics |
| `i18n/` (languages, policy, registry-resolver, `data/` prompts + 16 JSONs) | `doc::i18n` (embedded resources) | Port; `include_str!`/`include_bytes!` for data files |
| `config.py` | `app::config` (+ per-crate config structs) | Port schema as-is; same YAML, byte-identical defaults (D4 in doc 16) |
| `events.py`, `timing.py` | `engine::events` (tracing-backed) | Redesign on `tracing` + broadcast bus |
| `packages/cli/` (Typer app, all commands, model commands) | `cli` crate | Redesign on clap; same surface (04 §2) |
| `packages/backend/` (14 routers, services, emitters) | `gui` operations + `engine` services | Remove code; router list is the operation checklist (04 §10) |
| `apps/api/` (PG repo, Redis telemetry, arq) | — | Remove; no equivalents (SQLite + in-process) |
| `apps/desktop/backend/` (`LocalRuntime`, `ProgressHub`, `CredentialStore`, `UpdateGuard`, export routers) | `app` wiring + `engine` runtime + `store` | Port architecture; keyring/updater/dialogs via native crates |
| `packages/ui/` + `apps/web/` + `apps/desktop/frontend/` + `packages/shared-schema/` | `gui` crate | Remove; 13-screen inventory is the UX spec |
| `docker/`, `deploy/`, Tauri shell | — | Remove; salvage tray/updater/dialog patterns only |

## 4. Fixture strategy (the parity harness)

- **Prompt fixtures**: render every template in `i18n/data/tasks/` with fixed
  inputs; byte-compare against Rust rendering. Catches drift in the highest-risk
  asset (model behavior).
- **Unit fixtures**: glossary matcher cases, evidence-index queries, session
  transitions, punctuation rules, segmentation splits (incl. long-segment and
  MinerU `""`-target cases from the state invariants).
- **Run fixtures**: `FakeClient` end-to-end over one EPUB + one SRT sample in
  both translation modes; compare manifest, chapter JSON, glossary, usage
  ledger, usage append-once property under retried runs, and review shadow
  outputs.
- **Round-trip fixtures**: EPUB/DOCX in→out identity for untranslated content;
  TOC/anchor/style preservation diffs.

## 5. Seam order (incremental inside, cutover per crate)

Recommended order — each step is independently verifiable against fixtures:

1. `store` traits + SQLite (all other work stands on this).
2. `doc` pure layers first (`postprocess`, `prompts`, `i18n`, glossary matcher,
   markup), then readers/writers with round-trip fixtures.
3. `llm` transports + routing + retry (fixture-driven with fake server).
4. `engine` services in pipeline order (prepare → translate → review →
   autofix → report/assemble), SRT last (independent).
5. `cli` on clap against the engine; legacy `migrate-config`/`migrate-usage`
   become `import-legacy-state` (SQLite + YAML reader for old workspaces).
6. `gui` screens in nav order; event bus wired throughout.
7. `app` wiring (single-instance, tray, updater, dialogs, keyring).
8. Cutover: freeze Python, run full fixture suite, delete Python/TS/Tauri —
   all callers already migrated by construction (no shims survive).

Big-bang is rejected for the whole rewrite (unreviewable, unbisectable), but
per-crate cutover is required: no crate ships with two implementations behind
a flag. The seam order above keeps every intermediate state building and
tested.

## Sources

- `packages/core/wenyi_core/pipeline/` (all services listed in §3)
- `packages/core/wenyi_core/agents/`, `ingest/`, `assemble/`, `markup/`, `glossary/`, `review/`, `srt/`, `storage/`, `llm/`, `i18n/data/`, `postprocess/`, `document_styles/`, `pdf_bridge/client.py:17`
- `packages/core/wenyi_core/llm/registry.py:113-139`, `providers/fake.py:37`
- `packages/cli/`, `packages/backend/wenyi_backend/routers/`, `apps/api/`, `apps/desktop/`
- `AGENTS.md` (boundaries, state invariants, validation)

## Open questions

See 16-decisions-and-open-questions.md. Notable: fixture corpus size and
storage (sample books must stay out of the repo per `AGENTS.md` data rules),
legacy-state import scope (which CLI versions to support), and whether the
fake-server LLM harness needs wire-recording for provider quirks.
