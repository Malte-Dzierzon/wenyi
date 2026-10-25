# Decisions and Open Questions

Decision log for the Rust rewrite. Status is `DECIDED` (ratified by the mission verdicts), `PROPOSED` (recommendation awaiting ratification), or `UNRESOLVED` (needs investigation; each carries a resolution path). Evidence paths ground every entry.

## Decision log

### D1 — No web product in the target

- Status: DECIDED.
- Context: the current system ships two hosts (web API + desktop) sharing `packages/backend` contracts but no state. The rewrite targets a single local desktop binary.
- Decision: remove `apps/web`, `apps/api`, `docker/`, `deploy/`, and the PG/Redis/arq stack after parity (see `17-removal-plan.md`). No server mode, no multi-user scope.
- Evidence: `AGENTS.md` (Web/Desktop share contracts, not state); `apps/api/wenyi_api/db/schema.sql` (13 tables); `deploy/docker-compose.yml` (pg16/redis7); `packages/backend/wenyi_backend/`.

### D2 — Tauri shell removed, patterns salvaged

- Status: DECIDED.
- Context: `apps/desktop/src/` (9 modules, ~5.2k Rust) is a thin host: 7 commands, single-instance lock (`instance.rs`), tray (`main.rs:214-261`, `background.rs`), sidecar spawn (`process.rs`), `rfd` dialogs, signed updater (`updates.rs`), native drop/export.
- Decision: delete the Tauri shell at P8; port each behavior as a native-GUI equivalent (spec: doc 15 P6). Keep the module decomposition as the behavior checklist.
- Evidence: `apps/desktop/src/main.rs`, `instance.rs`, `background.rs`, `process.rs`, `updates.rs`, `native_drop.rs`, `native_export.rs`, `local_connection.rs`, `graphics.rs`.

### D3 — Modular Rust monolith, single binary, SQLite

- Status: DECIDED.
- Context: Python splits state across file-JSON (CLI), PG (web), and SQLite catalog + per-project SQLite (desktop).
- Decision: one binary; `crates/` workspace (domain crates + `cli` + `gui`); SQLite only — catalog database plus per-project SQLite files, preserving the desktop isolation shape (`apps/desktop/backend/wenyi_desktop/` local catalog + `SqliteStorage` per project). No PG/Redis/arq/key-value alternatives.
- Evidence: `packages/core/wenyi_core/storage/` (protocol/file/sqlite/artifacts/locks); `apps/desktop/backend/wenyi_desktop/` (catalog `local.sqlite3`, `LocalRuntime`); `apps/api/wenyi_api/db/schema.sql`.

### D4 — Config stays YAML with byte-identical defaults

- Status: DECIDED.
- Context: defaults live in `packages/core/wenyi_core/config.py::_DEFAULT_CONFIG_YAML`, mirrored in root `config.yaml` and both `docs/configuration.md` variants; AGENTS.md requires synchronized changes.
- Decision: Rust config parses the same YAML schema; P1 acceptance is a byte-identical render test of the defaults. No TOML/JSON migration — it would invalidate every existing user config for zero benefit.
- Evidence: `packages/core/wenyi_core/config.py`; `config.yaml`; `docs/configuration.md`.

### D5 — Provider architecture: trait + tiers + single-owner retry

- Status: DECIDED (pattern); provider list PROPOSED (see D5a).
- Context: `llm/` owns `base.py` (`LLMClient`), `router.py` tiers (strong/cheap/fast), `retrying.py` (sole retry owner; provider SDK retries off), `operations.py` + `registry.py`, per-provider modules, `FakeClient` (`llm/providers/fake.py`).
- Decision: port as `LlmClient` trait + tier router + one retry wrapper + per-provider protocol modules + scripted `FakeProvider`. Provider-specific fields stay in provider modules (AGENTS.md rule).
- Evidence: `packages/core/wenyi_core/llm/` (all files above); `packages/core/tests/fake_llm.py`.

### D5a — Which real providers to ship first

- Status: PROPOSED.
- Context: 10 provider kinds registered in `packages/core/wenyi_core/llm/registry.py:113-139`: deepseek, openai, openrouter, opencode-go, openai-compatible, orcarouter, ollama, vllm, gemini, fake.
- Proposal: ship OpenAI-compatible base + DeepSeek + Ollama (local) first; add the rest only on demand. Rationale: the compatible base covers OpenRouter/vLLM-shaped endpoints; Ollama covers offline-local, matching the Linux-first local story.
- Evidence: `packages/core/wenyi_core/llm/registry.py:113-139`; `packages/core/wenyi_core/llm/providers/`; `packages/core/wenyi_core/llm/configuration.py`.
- To ratify: usage telemetry or a user poll on which providers are actually configured in the wild.

### D6 — State migration scope: no legacy import

- Status: DECIDED.
- Context: desktop already refuses to read or migrate CLI/Web projects (AGENTS.md state invariants: isolated workspace).
- Decision: the Rust app starts with a fresh workspace and does not import CLI `state/`, PG data, or old desktop workspaces. Document the cutover; do not build migrators.
- Evidence: `AGENTS.md` (Desktop uses independent catalog, does not read/migrate CLI/Web).

### D7 — PDF strategy: keep the HTTP-isolated bridge, defer native rendering choice

- Status: PROPOSED with an UNRESOLVED sub-point (Q3).
- Context: PDF ingest spans `ingest/` readers, MinerU (empty-`""`-target semantics), `pdf_bridge/` HTTP-only BabelDOC client, and optional `weasyprint`/`fpdf2` output. AGENTS.md: BabelDOC stays process-isolated; no AGPL dependency in MIT code.
- Decision: keep the HTTP-only bridge boundary exactly (a Rust HTTP client replaces the Python one; still no AGPL code in-tree). Native PDF text extraction crate selection is Q3.
- Evidence: `packages/core/wenyi_core/pdf_bridge/`; `packages/core/wenyi_core/ingest/` (pdf paths); `AGENTS.md` (BabelDOC isolation rule).

### D8 — Interop policy: no shims, oracle-then-delete

- Status: DECIDED.
- Context: temptation to wrap Python pipeline stages behind Rust FFI/Subprocess during migration.
- Decision: no interop. The Python implementation runs side-by-side purely as a comparison oracle (same fixtures, diffed artifacts) until P8, then is deleted whole. Rationale: shims become permanent architecture; the phase plan (doc 15) never needs them because P2/P3 are LLM-free or fake-driven.
- Evidence: doc 15 phase graph; `17-removal-plan.md` preconditions.

### D9 — GUI: native Rust, Iced 0.14, Material-3-inspired minimal, Linux/Wayland-first

- Status: DECIDED (direction + framework: Iced 0.14, MIT).
- Context: current UI is React (`packages/ui` ~11.5k, 13 routes in `routes/manifest.tsx`, i18n en + zh-CN only) hosted thinly by web and desktop frontends.
- Decision: the 13-screen inventory is the UX spec; visual language is Material-3-inspired minimal; openSUSE Tumbleweed/Wayland is the first-class target. Framework is Iced 0.14: retained Elm architecture, tokio-native Command/Subscription async, full theming control, MIT license. Full 10-criterion evaluation (Slint 1.17, egui 0.36.2) in doc 10. Accepted trade-off: Iced has no production screen-reader support yet — mitigated by a central string table from day one, semantic widget trees, and a pre-1.0 accessibility re-evaluation gate (doc 10 §3).
- Evidence: `packages/ui/` (`App.tsx`, `routes/manifest.tsx`, `lib/api`, `ws.ts`); `apps/web/`; `apps/desktop/frontend/`; doc 10 toolkit table (versions verified 2026-10-25).

### D10 — Prompts + i18n data embedded as resources

- Status: DECIDED.
- Context: language rules and task templates live in `packages/core/wenyi_core/i18n/data/` (+ 16 language JSONs); CONTRIBUTING mandates English prompts with per-language examples preserved.
- Decision: embed via `include_str!`/resource bundle; byte-compare tests against the Python sources until P8. Prompt changes follow the 50k-word eval rule (doc 14 §3).
- Evidence: `packages/core/wenyi_core/i18n/`; `CONTRIBUTING.md`.

## Open questions (UNRESOLVED)

- **Q1 — GUI accessibility re-evaluation (pre-1.0 gate).** Iced 0.14 has no shipped screen-reader support (doc 10 §2, criterion 4). Resolve with: keep the string-table + semantic-widget discipline during P6; before 1.0, re-check Iced accessibility status and either adopt it or scope an alternative (assistive layer or toolkit revisit). Not a framework re-selection unless the gate fails.
- **Q2 — Eval book selection.** Need 1–2 public-domain novels ≥50k words with clean EPUBs (pre-1929, e.g. Gutenberg texts) fixed as the permanent before/after corpus. Resolve with: P4 owner picks and records checksums in the eval harness (doc 14 §5).
- **Q3 — PDF text-extraction crate.** Options include `pdf-extract`, `poppler` bindings, `mupdf` bindings (licensing check required), or shelling to `pdftotext`. Resolve with: P2 owner benchmarks text fidelity on three real PDFs vs the Python readers; licensing must stay MIT-compatible.
- **Q4 — Updater signing and distribution format.** Current: Tauri signed updater + AppImage/deb/rpm/NSIS/dmg (`.github/workflows/desktop.yml`). Single-binary Linux-first target suggests rpm + Flatpak and/or AppImage, but signing key custody and update-channel hosting are undecided. Resolve with: distribution decision before P6 ends; do not ship an unsigned auto-updater.
- **Q5 — Secret storage crate.** Current: Python `keyring` (Secret Service/kwallet/macOS/Windows per `local_credentials.py:41-56`). Rust candidates: `keyring-rs`/`oo7`. Resolve with: P6 spike verifying round-trip on Tumbleweed (Secret Service + kwallet6) with the same fallback discipline (memory-only + explicit warning).
- **Q6 — DOCX fidelity libraries.** Current: `python-docx` + custom `document_styles/docx.py`, `assemble/docx_blocks.py`, numbering/styles. Rust `docx-rs` covers common cases; style/numbering edge parity is unproven. Resolve with: golden-file diffing on the P2 corpus; gaps become explicit P2 acceptance caveats, not silent regressions.

## Sources

- `AGENTS.md`; `CONTRIBUTING.md`; root `config.yaml`; `LICENSE`
- `packages/core/wenyi_core/` (`config.py`, `storage/`, `llm/`, `i18n/`, `pdf_bridge/`, `ingest/`, `assemble/`, `glossary/`, `srt/`, `pipeline/`, `review/`, `markup/`, `document_styles/`)
- `packages/core/tests/`; `packages/core/tests/fake_llm.py`
- `packages/cli/`; `packages/backend/wenyi_backend/`; `packages/ui/`; `packages/shared-schema/`
- `apps/api/` (+ `db/schema.sql`, `tests/conftest.py`); `apps/web/`; `apps/desktop/` (src, backend, frontend)
- `docker/`; `deploy/`; `scripts/`; `.github/workflows/`; `docs/` (+ `zh/`)
