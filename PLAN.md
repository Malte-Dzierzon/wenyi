# Repository cleanup and migration tracker

> **Superseded.** The live plan, current slice status, dependency decisions, and
> migration gates now live in [`docs/RUST-REWRITE.md`](docs/RUST-REWRITE.md).
> This file is retained as a short historical marker; the detailed cleanup
> evidence remains in Git history.

## Completed cleanup (2026-11-10)

- Removed the web product (`apps/web`, `apps/api`, `packages/ui`,
  `packages/shared-schema`), the CLI package, the Tauri shell, Docker/deploy
  assets, and the JS toolchain.
- Retained the native GPUI app (`apps/desktop/native`), the temporary Python
  sidecar (`apps/desktop/backend`), the Python engine reference
  (`packages/core`), and the sidecar HTTP layer (`packages/backend`).
- Consolidated the retired `docs-next/` planning set into
  `docs/RUST-REWRITE.md` (English and Chinese).
- Established the Rust workspace with `crates/wenyi-core` and golden fixtures
  under `tests/golden/`.

## Where to look now

| Topic | Document |
|---|---|
| Vision, decisions, current slices, migration order | [Rust rewrite plan](docs/RUST-REWRITE.md) |
| Persistent Rust engineering rules | [Rust engineering rules](WENYI_RUST_ENGINEERING.md) |
| Implemented module boundaries | [Architecture](docs/architecture.md) |
| Engine behavior contract | [Engine contract](docs/cli.md) |
| Fork-specific history | [`docs/roadmap-custom.md`](docs/roadmap-custom.md) |
