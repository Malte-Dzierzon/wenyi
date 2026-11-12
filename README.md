<div align="center">

# Wenyi

**Whole-book understanding · Consistent terminology · Evidence-based review**

![](https://img.shields.io/github/last-commit/Malte-Dzierzon/wenyi?style=flat-square&color=16242d&labelColor=070e15)
[![License](https://img.shields.io/badge/license-MIT-D4B56A?style=flat-square&labelColor=070e15)](LICENSE)

<img src="docs/images/native-translation-overview.webp" alt="Native desktop app: translation overview with usage breakdown, run history, and chapter progress" width="1200">

<img src="docs/images/native-settings.webp" alt="Native desktop app: global settings with theme picker" width="1200">

[English](README.md) | [简体中文](docs/zh/README.md)

</div>

---

## Wenyi stack

| Layer | Software |
|---|---|
| Desktop UI | Rust `wenyi-native` ([gpui-kit 0.8](https://crates.io/crates/gpui-kit)) |
| Engine (current) | Python `wenyi-core` sidecar over authenticated loopback REST — the behavioral reference while the Rust engine is ported |
| Engine (target) | Rust `wenyi-core` in `crates/wenyi-core/`; slices 0–3 (config, documents, LLM, jobs) are implemented, pipeline parity is next |
| Models | 10 routed provider kinds — DeepSeek, OpenAI, OpenRouter, opencode-go, OrcaRouter, Ollama, vLLM, llama.cpp, Gemini, OpenAI-compatible — plus the offline `fake` provider |
| Storage | SQLite workspace (`local.sqlite3` + per-project `state.sqlite3`) |
| Formats | EPUB/TXT/Markdown/FB2/HTML/PDF/DOCX/SRT in · EPUB/TXT/HTML/Markdown/PDF/DOCX + SRT out |

Target: one native Rust app end to end — see the [Rust rewrite plan](docs/RUST-REWRITE.md) and [Rust engineering rules](WENYI_RUST_ENGINEERING.md).

---

## Quick start

```bash
uv sync --locked
export DEEPSEEK_API_KEY="YOUR_DEEPSEEK_API_KEY"
cargo run --manifest-path apps/desktop/native/Cargo.toml
```

The native UI spawns the Python sidecar automatically in development
(`WENYI_DESKTOP_PYTHON` selects the interpreter). Packaged builds bundle the
sidecar until the Rust engine replaces it.

Special cases:

- Desktop behavior, settings, workspace layout: [Desktop](docs/desktop.md).
- Engine behavior contract (formats, state layout, stages): [Engine contract](docs/cli.md).
- Engine behavior reference: [Pipeline](docs/pipeline.md) · [Configuration](docs/configuration.md) · [Architecture](docs/architecture.md).
- Current migration status and fork-specific deltas: [Rust rewrite plan](docs/RUST-REWRITE.md).

---

## Features

- **Whole-book understanding** — prescans the source before translation, building per-chapter digests and a book-level synopsis injected into every batch.
- **Real-time glossary** — extracts terms as translation progresses, with conflict detection and an optional background curator.
- **Multi-stage quality** — batch translation, optional polishing, and evidence-driven whole-book review.
- **Resumability** — batch checkpoints and chapter status tracking; reopen a project and continue.
- **Multiple LLM providers** — 10 routed kinds with per-operation tiers, retries, and usage accounting.
- **Formats** — EPUB, FB2, TXT, Markdown, HTML, PDF, DOCX, and SRT subtitle input; EPUB/TXT/HTML/Markdown/PDF/DOCX book output plus SRT subtitles.

<details>
<summary><b>Limitations</b></summary>

- Multilingual translation is experimental; real-model long-form quality needs further evaluation.
- Polishing and final review are the most expensive stages.
- PDF input defaults to MinerU and requires an API key for the initial conversion.
- SRT translation does not include a glossary, polishing, or whole-book review.
- Translation quality is bounded by the capabilities of the chosen LLM model.

</details>

---

## Repository

<details>
<summary><b>Apps</b></summary>

- `apps/desktop/native/` — Rust GPUI desktop UI (`wenyi-native`)
- `apps/desktop/backend/` — `wenyi_desktop` Python sidecar (temporary reference)

</details>

<details>
<summary><b>Packages and crates</b></summary>

- `crates/wenyi-core/` — Rust engine: config, documents, glossary, i18n, jobs, LLM, store (pipeline parity in progress)
- `packages/core/` — `wenyi-core` Python engine reference: pipeline, glossary, LLM routing, ingest/assemble
- `packages/backend/` — HTTP routes and app services used by the sidecar

</details>

<details>
<summary><b>Docs</b></summary>

- [Rust rewrite plan](docs/RUST-REWRITE.md) · [Rust engineering rules](WENYI_RUST_ENGINEERING.md)
- [Desktop](docs/desktop.md) · [Configuration](docs/configuration.md) · [Pipeline](docs/pipeline.md) · [Architecture](docs/architecture.md) · [Engine contract](docs/cli.md)
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — development guidelines

</details>

---

## About this fork

Fork of [BigDawnGhost/wenyi](https://github.com/BigDawnGhost/wenyi) with full
history; upstream stays the reference for the shipped Python codebase.

Direction: a single native Rust desktop app, no web product. The Rust UI still
runs on the Python sidecar while `crates/wenyi-core` is ported slice by slice.
Current status, decisions, and fork-specific behavior (parallel chapter workers,
glossary curator, llama.cpp provider) are tracked in the
[Rust rewrite plan](docs/RUST-REWRITE.md).

---

## License

[MIT](LICENSE) — Copyright (c) 2026 BigDawnGhost.
