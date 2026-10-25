# Decisions and Open Questions

Decision log for the Rust rewrite. Status is `DECIDED` (ratified by the mission verdicts), `PROPOSED` (recommendation awaiting ratification), or `UNRESOLVED` (needs investigation; each carries a resolution path). Evidence paths ground every entry.

## Decision log

### D1 — No web product in the target

- Status: DECIDED.
- Decision: remove `apps/web`, `apps/api`, `docker/`, `deploy/`, and the
  PG/Redis/arq stack after parity (doc 17). No server mode, no multi-user scope.
- Evidence: `apps/api/wenyi_api/db/schema.sql` (13 tables);
  `deploy/docker-compose.yml` (pg16/redis7); `packages/backend/wenyi_backend/`.

### D2 — Tauri shell removed, patterns salvaged

- Status: DECIDED.
- Decision: delete the Tauri shell at P8; port each behavior as a native-GUI
  equivalent (doc 15 U5; checklist in doc 18 §4). Keep the module decomposition as the checklist.
- Evidence: `apps/desktop/src/` (9 modules: main, instance, background,
  process, updates, native_drop, native_export, local_connection, graphics).

### D3 — Modular Rust monolith, single binary, SQLite

- Status: DECIDED.
- Decision: one binary; `crates/` workspace; SQLite only — catalog database
  plus per-project SQLite files (desktop isolation shape). No PG/Redis/arq.
- Evidence: `packages/core/wenyi_core/storage/`; `apps/desktop/backend/`;
  `apps/api/wenyi_api/db/schema.sql`.

### D4 — Config stays YAML with byte-identical defaults

- Status: DECIDED.
- Decision: Rust config parses the same YAML schema; P1 acceptance is a
  byte-identical render test of the defaults. No TOML/JSON migration.
- Evidence: `packages/core/wenyi_core/config.py`; `config.yaml`.

### D5 — Provider architecture: trait + tiers + single-owner retry

- Status: DECIDED (pattern); provider list PROPOSED (D5a).
- Decision: port as `LlmClient` trait + tier router + one retry wrapper +
  per-provider protocol modules + scripted `FakeProvider`. Provider-specific
  fields stay in provider modules.
- Evidence: `packages/core/wenyi_core/llm/`; `packages/core/tests/fake_llm.py`.

### D5a — Which real providers to ship first

- Status: PROPOSED.
- Proposal: ship OpenAI-compatible base + DeepSeek + Ollama (local) first; add
  the rest only on demand. The compatible base covers OpenRouter/vLLM-shaped
  endpoints; Ollama covers offline-local.
- Evidence: `llm/registry.py:113-139` (10 kinds); `llm/providers/`.
- To ratify: usage data or user poll on configured providers.

### D6 — State migration scope: no legacy import

- Status: DECIDED.
- Decision: fresh workspace; no import of CLI `state/`, PG data, or old desktop
  workspaces. Document the cutover; do not build migrators.
- Evidence: `AGENTS.md` (desktop isolation: no CLI/Web migration today).

### D7 — PDF strategy: keep the HTTP-isolated bridge

- Status: PROPOSED (Q3: native extraction crate).
- Decision: keep the HTTP-only bridge boundary exactly (Rust HTTP client
  replaces Python; no AGPL code in-tree).
- Evidence: `pdf_bridge/`; `AGENTS.md` (BabelDOC isolation).

### D8 — Interop policy: no shims, oracle-then-delete

- Status: DECIDED.
- Decision: no FFI/subprocess interop. Python runs side-by-side purely as a
  comparison oracle until P8, then is deleted whole.
- Evidence: doc 15 phase graph; doc 17 preconditions.

### D9 — GUI framework: GPUI-Kit 0.8 (DECIDED 2026-10-25, U0 passed)

- Status: DECIDED.
- Decision: GPUI-Kit 0.8 (Apache-2.0, `gpui-pre =0.3.9`, no fork) as the UI
  framework, talking to the existing desktop backend over its loopback
  REST+WS contract (doc 18 §3) until the Rust engine lands. Iced 0.14 stays
  the documented fallback (doc 10 §2). Do not build both.
- Evidence: U0 spike results (doc 10 §3: window + Button + Input + tokio
  bridge + headless test all green on Tumbleweed/Wayland);
  [longbridge/gpui-kit](https://github.com/longbridge/gpui-kit).

### D10 — Prompts + i18n data embedded as resources

- Status: DECIDED.
- Decision: embed via `include_str!`/resource bundle; byte-compare tests
  against Python sources until P8. Prompt changes follow the 50k-word eval
  rule (doc 14 §3).
- Evidence: `packages/core/wenyi_core/i18n/`; `CONTRIBUTING.md`.

## Open questions (UNRESOLVED)

- **Q0 — Framework spike: CLOSED (PASS 2026-10-25).** All four items green, see doc 10 §3. Residual: live IME composition untested (bindings confirmed) — retest in U3 with real CJK input.
- **Q1 — Accessibility: satisfied by GPUI-Kit.** AccessKit built in; keep semantic-widget discipline. (Was Iced-fallback-only gate; moot as a blocker.)
- **Q2 — Eval corpus.** 1–2 public-domain novels ≥50k words with clean EPUBs; P4 owner picks and records checksums (doc 14 §5).
- **Q3 — PDF text-extraction crate.** `pdf-extract`, poppler/mupdf bindings (license check), or `pdftotext` shell-out. P2 owner benchmarks fidelity vs Python readers; must stay MIT-compatible.
- **Q4 — Updater signing + distribution.** Decide before U5 ends: RPM + Flatpak/AppImage suggested; never ship an unsigned auto-updater.
- **Q5 — Secret storage crate.** `keyring`/`oo7`; U5 spike must verify round-trip on Tumbleweed (Secret Service + kwallet6) with memory-only fallback + explicit warning.
- **Q6 — DOCX fidelity.** `docx-rs` style/numbering parity unproven; golden-file diffing on the P2 corpus, gaps become explicit acceptance caveats.

## Sources

- `AGENTS.md`; `CONTRIBUTING.md`; root `config.yaml`; `LICENSE`
- `packages/core/wenyi_core/` (`config.py`, `storage/`, `llm/`, `i18n/`, `pdf_bridge/`, `ingest/`, `assemble/`, `glossary/`, `srt/`, `pipeline/`, `review/`, `markup/`, `document_styles/`)
- `packages/core/tests/`; `packages/core/tests/fake_llm.py`
- `packages/cli/`; `packages/backend/wenyi_backend/`; `packages/ui/`; `packages/shared-schema/`
- `apps/api/` (+ `db/schema.sql`, `tests/conftest.py`); `apps/web/`; `apps/desktop/` (src, backend, frontend)
- `docker/`; `deploy/`; `scripts/`; `.github/workflows/`; `docs/` (+ `zh/`)
