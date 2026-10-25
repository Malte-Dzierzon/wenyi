# Wenyi Rust-Native Rewrite — Plan Index

This directory (`docs-next/`) holds the planning docs for rewriting Wenyi as a
Rust-native desktop app. It does not describe the current code except where a
doc explicitly audits it. The current-code docs remain in `docs/`.

## How to read the plan

Read in this order: **audit → target → roadmap**.

1. **Audit** (docs 02–03): what exists today and what is worth keeping.
2. **Target** (docs 04–09): what the Rust system must look like and do.
3. **Roadmap** (docs 10–17): how to get there, in what order, with what checks.

| # | Doc | Scope |
|---|-----|-------|
| — | `README.md` | This index: reading order and conventions. |
| 01 | `01-project-vision.md` | End-state, non-goals, users/workflows, principles, success criteria. |
| 02 | `02-current-architecture.md` | Verified current system: components, dependencies, storage, transports. |
| 03 | `03-codebase-audit.md` | Per-subsystem audit: behavior, verdict (port/remove), risks, migration path. |
| 04 | `04-feature-inventory.md` | Verified feature list: current behavior and target disposition. |
| 05 | `05-target-architecture.md` | Rust monolith layout: crates, layering rules, dependency direction. |
| 06 | `06-rust-migration.md` | Port-vs-redesign rules, module→crate map, interop policy, seam order. |
| 07 | `07-translation-engine.md` | Pipeline stage port: preparation → translation → review → finalization. |
| 08 | `08-llm-providers.md` | LLM router, 10 provider kinds, retry/usage, target provider trait. |
| 09 | `09-document-processing.md` | Ingest/assemble format semantics; EPUB-first crate design. |
| 10 | `10-desktop-gui.md` | Toolkit pick, screen plan, GUI architecture, visual direction, packaging. |
| 11 | `11-configuration.md` | Config audit: per-item disposition and target config design. |
| 12 | `12-data-and-persistence.md` | SQLite storage design: catalog + per-project schema, migration story. |
| 13 | `13-reliability-and-performance.md` | Resume/checkpoint/recovery, job queue, perf budgets, diagnostics. |
| 14 | `14-testing-and-validation.md` | Test strategy: parity fixtures, golden files, eval harness, gates. |
| 15 | `15-migration-roadmap.md` | Ordered build phases with entry/exit criteria. |
| 16 | `16-decisions-and-open-questions.md` | Decision log and unresolved questions. |
| 17 | `17-removal-plan.md` | Explicit cut list: what is removed after parity and why. |

## Conventions

- **Status labels.** Each target/roadmap doc marks items `DECIDED` (locked),
  `PROPOSED` (needs review, tracked in doc 16), or `AUDITED` (verified against
  current code with a path citation).
- **Path citations.** Every architectural claim cites an existing path as
  `path:line` or `path + symbol` (e.g. `packages/core/wenyi_core/pipeline/orchestrator.py`
  + `Orchestrator`). Unverifiable statements are marked `[INFERENCE]`.
- **Mermaid.** Used only where a diagram carries structure text cannot.
- **Cross-links.** Docs link to each other instead of repeating content; doc 16
  collects open questions (each doc also lists its own at the end under
  `## Open questions`).

## Sources

- `README.md` (fork direction statement, docs list)
- `docs/architecture.md` (current module map)
- `AGENTS.md` (constraint doc)

## Open questions

See `16-decisions-and-open-questions.md`.
