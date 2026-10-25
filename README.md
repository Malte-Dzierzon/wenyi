<div align="center">

<h1>
  <img src="docs/images/wenyi-emblem.png" alt="" width="280">
  <br>
  <img src="docs/images/wenyi-wordmark-en.svg" alt="Wenyi" width="180" height="54">
</h1>

**Carry stories across languages.**

A desktop app for translating books and long-form writing, with the whole work in view.

Whole-book understanding · Consistent terminology · Evidence-based review

[![Tests](https://img.shields.io/github/actions/workflow/status/Malte-Dzierzon/wenyi/tests.yml?style=flat-square&labelColor=00263D)](https://github.com/Malte-Dzierzon/wenyi/actions/workflows/tests.yml)
[![License](https://img.shields.io/badge/license-MIT-D4B56A?style=flat-square&labelColor=00263D)](LICENSE)

</div>

---

## About this fork

This repository is a fork of [BigDawnGhost/wenyi](https://github.com/BigDawnGhost/wenyi), preserved with its full Git history. The upstream project remains the reference for the current codebase.

The direction of this fork is a **native desktop application** with a **Rust backend and native Rust desktop GUI**:

- One native desktop app; no separate web application or browser-based frontend.
- Minimal, maintainable architecture.
- Focus on the existing book-translation functionality.

> **Status:** The refactor has not started. The code in this repository is still the
> upstream Python/TypeScript implementation, including the web frontend. Everything
> below describes the code as it exists today, not the planned Rust rewrite.

---

## Current functionality

Wenyi translates long-form texts — novels, monographs, narrative nonfiction — with the whole work in view:

- **Whole-book understanding** — prescans the source before translation, building per-chapter digests and a book-level synopsis injected into every batch.
- **Real-time glossary** — extracts terms as translation progresses, with conflict detection.
- **Multi-stage quality** — batch translation, optional polishing, and evidence-driven whole-book review.
- **Resumability** — batch checkpoints and chapter status tracking; reopen a project and continue.
- **Multiple LLM providers** — DeepSeek, OpenAI, OpenRouter, Google Gemini, Ollama, vLLM, and generic OpenAI-compatible endpoints.
- **Formats** — EPUB, FB2, TXT, Markdown, HTML, PDF, DOCX, and SRT subtitle input; EPUB/TXT/HTML/Markdown/DOCX/SRT output.

See the [translation pipeline](docs/pipeline.md) for stage details.

---

## Quick start (current code)

With Python 3.10+ and [uv](https://docs.astral.sh/uv/) installed, run from the repository root:

```bash
uv sync --locked
export DEEPSEEK_API_KEY="YOUR_DEEPSEEK_API_KEY"
uv run wenyi translate book.epub
```

For the graphical app, downloads, and setup, see the [Desktop guide](docs/desktop.md#quick-start).

---

## Documentation (current code)

- [Desktop guide](docs/desktop.md) — installation/builds, API keys, native import and save, local data
- [CLI guide](docs/cli.md) — commands, translation, resume, and export
- [Web deployment](docs/web.md) — upstream's optional self-hosted browser workspace (not part of this fork's planned direction)
- [Configuration](docs/configuration.md) — providers, languages, pipeline switches, segmentation, paths
- [Translation pipeline](docs/pipeline.md) — whole-book analysis, terminology, context, polishing, review
- [Architecture](docs/architecture.md) — upstream module responsibilities (pre-refactor)
- [Contributing](CONTRIBUTING.md) — upstream development guidelines

---

## Limitations

- Multilingual translation is experimental; real-model long-form quality needs further evaluation.
- Polishing and final review are the most expensive stages.
- PDF input defaults to MinerU and requires an API key for the initial conversion.
- SRT translation does not include a glossary, polishing, or whole-book review.
- Translation quality is bounded by the capabilities of the chosen LLM model.

---

## License

[MIT](LICENSE) — Copyright (c) 2026 BigDawnGhost. See [LICENSE](LICENSE) for the full text.
