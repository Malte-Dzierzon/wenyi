# Contributing to Wenyi

**English** | [简体中文](docs/zh/CONTRIBUTING.md)

Thank you for helping improve Wenyi. The project prioritizes the quality and reliability of long-form novel translation. The current direction is the [Rust rewrite plan](docs/RUST-REWRITE.md): Python code is a read-only behavioral reference; new engine work happens in Rust.

## What you can contribute

Contributions are welcome in the following areas:

- Native desktop UI (`apps/desktop/native/`): GPUI screens, widgets, and app wiring.
- Rust engine (`crates/wenyi-core/`, per the rewrite plan): documents, glossary, LLM providers, pipeline, jobs, storage.
- Reference behavior fixes in Python (only where the oracle itself is wrong): input parsing, pipeline, export, with regression tests.
- Tests: real-world failure cases, regression tests, offline fake-LLM tests, and Rust golden fixtures.
- Documentation: usage instructions, configuration explanations, troubleshooting, and translations.

Changes to the core translation pipeline can affect translation quality in subtle ways. Before proposing such a change, test it on a public-domain novel of at least 50,000 words and include a before-and-after comparison that explains the quality impact.

Use standard English for code comments, docstrings, configuration comments, UI messages, and prompt instructions. Keep language rules and task templates in `packages/core/wenyi_core/i18n/data/` (embedded into Rust via `include_str!`). Generated prose, including glossary notes and analysis descriptions, follows the translation target; original `source` and `aliases` remain unchanged for matching. Preserve language-specific examples and multilingual test fixtures, and keep English and Chinese documentation synchronized.

## Python regression tests

Keep each rule at its owning layer: Core tests cover domain rules and persistence/resume invariants; shared backend tests cover platform-neutral application and HTTP contracts; Desktop tests cover adapter wiring, platform storage, queues, and native file safety. Orchestrator tests should retain cross-service workflows rather than duplicate lower-level service matrices. Share fixtures through `conftest.py` and helpers through support modules, not imports from other `test_*` modules.

From the repository root:

```bash
uv sync --locked --all-packages --group dev
uv run --no-sync python -c "import tiktoken; tiktoken.get_encoding('cl100k_base')"
uv run --no-sync pytest -q
```

The vocabulary preparation command downloads `cl100k_base` if it is not cached yet; prepare it while online before running offline tests. Tests keep the real tokenizer rather than replacing token-budget semantics with a fake.

## Rust tests

From the repository root:

```bash
cargo test --locked --manifest-path apps/desktop/native/Cargo.toml
```

Engine tests live with `crates/wenyi-core/` once it exists. Rust tests use a scripted fake provider; CI never calls real model APIs.
