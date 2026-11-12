# Fork history — custom changes

> **Historical note.** This file used to be a German development log written
> between 2026-11-04 and 2026-11-06, when the plan still assumed a Python
> backend. That premise is obsolete: the project is now a native Rust rewrite.
> The current plan, status, and migration gates live in
> [Rust rewrite plan](RUST-REWRITE.md). The original log remains in Git history.

## Verified fork additions carried forward

| Area | Change | Where it lives now |
|---|---|---|
| Parallel work | Three workflow lanes translate different chapters concurrently; per-chapter locks and short state locks prevent lost updates. | [Architecture — parallel chapter workers](architecture.md#parallel-chapter-workers-custom-fork) |
| Glossary curator | Background `glossary_curator` job checks ~20 terms per pass against book evidence and proposes conflicts instead of overwriting terms. | `packages/core/wenyi_core/pipeline/curator.py`; [Configuration — operations](configuration.md#registered-operations) |
| Local models | `llamacpp` provider and preset for an OpenAI-compatible gateway on `127.0.0.1:8093`. | `packages/core/wenyi_core/llm/providers/llamacpp.py` |
| Style priority | Explicit user style rules take precedence over generated observations. | Style analysis and Review prompts in `packages/core/wenyi_core/agents/` |
| Native UI | GPUI screens replace the former React/Tauri interface. | `apps/desktop/native/` |

## Superseded assumptions

- “Backend stays Python” — no; the Python engine is a read-only oracle until the
  Rust engine reaches parity.
- “No Rust rewrite of the backend” — no; `crates/wenyi-core` is the target.
- “Web UI shares the workbench” — no; the web product was removed.
- “Next focus: curator v1” — the curator is implemented; the next slice is
  one-book translation parity.

For the old step-by-step reasoning, use `git log -- docs/roadmap-custom.md`.
