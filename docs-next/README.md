# Wenyi Native — Plan Index

Planning docs for the Rust-native desktop rewrite. Current-code docs stay in `docs/`.

## The five questions

| # | Question | Answer |
|---|---|---|
| 1 | What exists today? | `02-current-architecture.md` (system map) + `03-codebase-audit.md` (per-subsystem verdicts) |
| 2 | What does the app need? | `01-project-vision.md` (scope) + `04-feature-inventory.md` (verified capabilities) |
| 3 | What does the final architecture look like? | `05-target-architecture.md` (monolith, crates, GUI, storage, providers) |
| 4 | What can be removed? | `17-removal-plan.md` (ordered cut list) + `03-codebase-audit.md` (verdicts) |
| 5 | Smallest implementation sequence? | `15-migration-roadmap.md` (UI-first U0–U6, then engine P1–P5/P7–P8) |

## All docs

| # | Doc | Scope |
|---|-----|-------|
| — | `README.md` | This index. |
| 01 | `01-project-vision.md` | End-state, non-goals, workflows, success criteria. |
| 02 | `02-current-architecture.md` | Verified current system: components, dependencies, storage, transports. |
| 03 | `03-codebase-audit.md` | Per-subsystem audit: behavior, verdict (port/remove), risks, migration path. |
| 04 | `04-feature-inventory.md` | Verified feature list: current behavior and target disposition. |
| 05 | `05-target-architecture.md` | Rust monolith: crates, GUI, storage, providers, reliability, config. |
| 06 | `06-rust-migration.md` | Port-vs-redesign rules, module→crate map, fixture parity, seam order. |
| 07 | `07-translation-engine.md` | Pipeline stages, context strategy, glossary, review/QA, acceptance criteria. |
| 08 | `08-llm-providers.md` | 10 provider kinds, tier routing, retry/usage, target provider trait. |
| 09 | `09-document-processing.md` | Format implementations, EPUB-first design, round-trip tests. |
| 10 | `10-desktop-gui.md` | Toolkit decision (GPUI-Kit 0.8), spike results, screen plan, packaging. |
| 11 | `11-configuration.md` | Config audit and target YAML design. |
| 12 | `12-data-and-persistence.md` | SQLite storage design: catalog + per-project schema, migration story. |
| 13 | `13-reliability-and-performance.md` | Resume/checkpoint/recovery, job queue, perf budgets, diagnostics. |
| 14 | `14-testing-and-validation.md` | Test strategy: parity fixtures, golden files, eval harness, gates. |
| 15 | `15-migration-roadmap.md` | UI-first phases U0–U6, then engine P1–P5/P7–P8, with entry/exit criteria. |
| 16 | `16-decisions-and-open-questions.md` | Decision log (D1–D10) and open questions (Q1–Q6; Q0 closed). |
| 17 | `17-removal-plan.md` | Explicit cut list with preconditions and verification. |
| 18 | `18-desktop-parity.md` | Desktop UI inventory, backend op list, GPUI-Kit mapping, parity checklists. |

Read audit (01–04) → target (05–14) → execution (15–18).

## Conventions

- **Path citations.** Architectural claims cite `path:line` or `path + symbol`.
  Unverifiable statements are marked `[INFERENCE]`.
- **Status labels.** `DECIDED` (locked), `PROPOSED` (in doc 16), `AUDITED`
  (verified with a path citation). Feature rows use the doc 04 vocabulary.
- **Cross-links, not repetition.** Docs link instead of restating; doc 16
  collects open questions.

## Sources

- `AGENTS.md`, `docs/architecture.md`, `README.md`
