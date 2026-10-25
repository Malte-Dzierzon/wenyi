# Testing and Validation

Current test inventory, the mock policy that makes offline testing possible, and the target test strategy for the Rust rewrite, phase by phase.

## 1. Current inventory (verified October 2026)

| Suite | Location | Size | What it covers |
|---|---|---|---|
| Core domain tests | `packages/core/tests/` | 82 `test_*.py` files (+ `fake_llm.py`, `conftest.py`, fixture helpers) | Pipeline, agents, ingest, assemble, glossary, review, llm routing, i18n, storage, SRT, architecture boundaries |
| Backend contract tests | `packages/backend/tests/` | 20 files | Platform-neutral HTTP contracts, context isolation, export admission, worker tasks |
| Web API tests | `apps/api/tests/` | ~20 test modules | PG adapters, route isolation, queues, retention, segment history |
| Desktop backend tests | `apps/desktop/backend/tests/` | ~14 test modules | Catalog recovery, local runtime, credentials, updates, offline guards, PE/sidecar entry |
| Web E2E | `apps/web/tests/` (+ `playwright.config.ts`) | 37 `*.spec.ts` | 13-screen flows, dialogs, layout |
| Desktop frontend E2E | `apps/desktop/frontend/tests/` | 26 spec files | Shell, sidebar, export, native drop |
| Rust inline tests | `apps/desktop/src/*.rs` | `#[test]`s in 8 of 9 modules | Instance lock, process spawn, updates, native export/drop |
| Boundary tests | `packages/core/tests/test_architecture_boundaries.py`, `test_orchestrator_contract.py` | 2 files | Module dependency direction, Orchestrator facade thinness (AGENTS.md mandates running these on boundary changes) |

CI (`.github/workflows/tests.yml`): Python 3.10 + 3.12 matrix, PostgreSQL 16 + Redis 7 services, `ruff check` + `ruff format --check`, offline selection first, then integration selection, then wheel build with bundled-resource verification (`scripts/check_wheel_resources.py`). Desktop packaging is a separate workflow (`.github/workflows/desktop.yml`: linux-x64 AppImage/deb/rpm, windows-x64 NSIS, macos-arm64 dmg).

## 2. Mock and isolation policy (must be preserved)

- **FakeClient.** `packages/core/wenyi_core/llm/providers/fake.py` defines `FakeClient(LLMClient)` with a scripted handler; `packages/core/tests/fake_llm.py` routes full-pipeline agent tasks through it offline. `METERED_TOTAL_TOKENS = 8` per call keeps usage-ledger tests deterministic. The Rust port needs an equivalent scripted `FakeProvider` from day one of P3 — every pipeline test runs against it, never against the network.
- **Integration gating.** `apps/api/tests/conftest.py:75-89`: anything depending on the `pg_pool` fixture is auto-marked `integration`; offline run is `pytest -m "not integration"`, service run is `-m integration` with `WENYI_TEST_DATABASE_URL` / `WENYI_TEST_REDIS_URL` pointing at isolated services. Missing services produce **skips, never passes**. The Rust workspace should copy this split exactly: `cargo test` offline by default, `cargo test --features integration` (or an env-gated harness) for anything touching the network or real services.
- **Real tokenizer.** CONTRIBUTING keeps the real `cl100k_base` tokenizer in tests (vocabulary prepared while online) instead of faking token-budget semantics. The Rust port should use the real `tiktoken-rs` encoding in budget tests for the same reason.
- **Layer ownership** (CONTRIBUTING): Core tests cover domain rules and persistence/resume invariants; backend tests cover HTTP contracts; app tests cover adapter wiring and native safety. Orchestrator-level tests keep cross-service workflows, not duplicated service matrices. Mirror this in Rust: domain crate tests, thin CLI/GUI wiring tests, no duplication.

## 3. Quality evaluation via public-domain texts

CONTRIBUTING requires: before changing the core translation pipeline, test on a public-domain novel of **at least 50,000 words** and include a before/after quality comparison. There is no automated LLM-quality gate in CI today — evaluation is a human process documented in the PR. The Rust rewrite must not regress this: any phase that ports translation-affecting logic (P4, P5) needs a recorded before/after run on a public-domain book (e.g. a ~50k+ word Gutenberg text), judged by a human, with the comparison linked from the phase acceptance record. See §5 for the harness that makes this cheap.

## 4. Target test strategy per phase

**P1 foundation.** Deterministic unit tests for config parsing (default YAML byte-identical to `packages/core/wenyi_core/config.py::_DEFAULT_CONFIG_YAML`), storage round-trips (SQLite artifact keys with `/` separators, atomic `os.replace`-equivalent commit), logging redaction (no key material in logs — negative test with a canary secret).

**P2 document processing (LLM-free).** Fixture-based parity corpus: a set of small EPUB/FB2/TXT/DOCX/HTML/SRT inputs with byte-recorded expected outputs. EPUB round-trip is the flagship: input EPUB → segment → reassemble → output EPUB must preserve spine, NCX/nav, annotations/anchors, and metadata (semantics owned by `packages/core/wenyi_core/ingest/` + `assemble/` + `markup/`). Golden-file tests fail on any byte diff; updating a golden file requires human review of the diff. Segment IDs must be stable across re-runs (state invariant).

**P3 providers.** Scripted `FakeProvider` tests for every tier route (strong/cheap/fast), retry behavior (single owner — port of `packages/core/wenyi_core/llm/retrying.py`, provider SDK retries off), error mapping (user-facing errors typed, no traceback leakage), and usage accounting (append-once ledger, no double-count on retry/resume).

**P4 pipeline + glossary.** Golden-file tests for glossary matching/extraction/conflict resolution (port of `packages/core/wenyi_core/glossary/` + `tests/test_glossary*.py`); Resume tests: kill mid-run, re-run, assert completed batches/chapters skipped and usage not double-counted. `target None` vs `""` semantics covered explicitly. Translation-context tests (rolling context window, title translation batching) run against `FakeProvider` with recorded multi-turn scripts.

**P5 review/polish/QA.** Shadow-only invariant test: run review, assert formal chapter `target`s byte-identical until autofix publishes via the recoverable index. Evidence-index and conflict-detection tests ported from `packages/core/tests/test_review*.py`. Human-judged before/after eval on a public-domain book (see §3).

**P6 GUI.** Headless GUI tests for project lifecycle and progress rendering (no LLM: drive against `FakeProvider` + fixture book); platform tests for single-instance lock, tray presence, file dialogs, updater signature check — ported from the Rust inline tests in `apps/desktop/src/` and the Playwright desktop specs.

**P7 CLI + integration.** CLI surface tests asserting command/flag parity with `packages/cli` (spec in the target-CLI doc); end-to-end run of a fixture book through import → translate (fake) → review → export, comparing artifacts against the Python implementation's output on the same fixture.

**P8 removal.** Deletion checklist tests: after each component is removed, workspace-wide grep for its imports/symbols must return empty; final `cargo build` + full offline suite green with no Python/Node toolchain present.

## 5. LLM-quality eval harness (new, small)

Build once in P4, reuse in P5/P7: a CLI subcommand (or test-only binary) that runs a fixed public-domain excerpt through translate (+ review/polish) with a pinned model config and writes side-by-side before/after artifacts (source, old-output, new-output, diff). Human judges score; the harness only guarantees reproducibility (pinned prompts, temperatures, fixture inputs), not quality. Keep it under a few hundred lines — it is scaffolding for judgment, not a metric to game.

## 6. Acceptance criteria template (per phase)

Each phase's acceptance record must state: (a) ported test list with source-Python-test traceability, (b) new tests added and why, (c) golden files added/updated with reviewer sign-off on diffs, (d) offline suite green on a clean checkout, (e) quality-eval result where applicable (P4/P5/P7), (f) known gaps carried to which later phase. A phase is done when all six are recorded, not when code compiles.

## Sources

- `packages/core/tests/` (82 files; `fake_llm.py`, `test_architecture_boundaries.py`, `test_orchestrator_contract.py`)
- `packages/core/wenyi_core/llm/providers/fake.py` (`FakeClient`)
- `packages/backend/tests/` (20 files); `apps/api/tests/conftest.py:75-89` (integration auto-mark); `apps/desktop/backend/tests/`
- `apps/web/tests/` (37 specs); `apps/desktop/frontend/tests/` (26 files); `apps/desktop/src/*.rs` (inline tests)
- `CONTRIBUTING.md` (50k-word rule, layer ownership, tokenizer policy); `AGENTS.md` (boundary-test mandates)
- `.github/workflows/tests.yml`, `.github/workflows/desktop.yml`

## Open questions

See `16-decisions-and-open-questions.md` (notably GUI test harness choice and eval-book selection).
