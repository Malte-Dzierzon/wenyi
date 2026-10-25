# Removal Plan

What gets deleted, in what order, and what must be proven first. Iron rule: **nothing is removed before it is migrated and validated** — each item's precondition names the parity proof, and the verification step makes the deletion checkable.

## Order (dependency-safe)

| # | Component | Precondition (parity proof) | Verification | Risk |
|---|---|---|---|---|
| 1 | `apps/web` (+ its `tests/`, `playwright.config.ts`) | P6 GUI covers the 13-screen inventory (`packages/ui/routes/manifest.tsx`); headless GUI tests green | `git grep` for `@wenyi/ui` web-host imports empty; web E2E suite retired with sign-off | Dropping a screen nobody ported — mitigate with the screen-by-screen checklist in P6 acceptance |
| 2 | `packages/ui` + `packages/shared-schema` | Same as 1, plus no remaining TS consumer of `api.d.ts` | Workspace `grep` for `from '@wenyi/` and `shared-schema` imports empty; `pnpm` workspace has no members left | Shared validation logic hiding in TS — audit `lib/` for rules without a Rust counterpart before deleting |
| 3 | `apps/api` (+ PG schema, arq workers) + `packages/backend` HTTP layer | P7: CLI + GUI drive the full lifecycle locally; integration report shows no capability only the server had | `grep -ri 'wenyi_api\|wenyi_backend\|arq\|psycopg\|redis'` outside docs empty; PG/Redis services removed from CI matrix | Undocumented operational capability (retention jobs, telemetry in `apps/api`) — inventory `wenyi_api/` adapters before deletion |
| 4 | `docker/` + `deploy/` + nginx config | 3 done (nothing left to deploy); single-binary packaging (P7) published | `docker/`, `deploy/` gone; CI has no image-build jobs; release workflow ships the binary | Users depending on compose files — announce deprecation one release ahead |
| 5 | Python product code: `packages/core/wenyi_core`, `packages/cli`, `packages/backend`, `apps/desktop/backend` (+ all Python tests) | P2–P7 acceptance records complete: golden corpora, resume tests, CLI parity diff empty, eval comparisons recorded | `find . -name '*.py' -not -path './docs/*'` returns only tooling explicitly kept; `uv` workspace removed; full `cargo test` green with no Python toolchain installed | Oracle loss — keep the last Python tag reachable in git history (no separate archive needed; history is the archive) |
| 6 | Tauri shell: `apps/desktop/src/`, `src-tauri/`, `apps/desktop/frontend` | P6 platform tests (single-instance, tray, dialogs, updater, keyring) green on the native GUI | `grep -ri 'tauri'` outside docs empty; no `*_tauri*` configs; release workflow builds only the native binary | Updater-channel continuity for existing installs — Q4 decision must land before this step |
| 7 | Toolchains: `uv.lock`, `pyproject.toml`s, `pnpm-workspace.yaml`, `package.json`s, Node/Python CI jobs | 1–6 done and the tree builds + tests green with only `cargo` | Fresh-container check: clone, `cargo build`, `cargo test` (offline) with no Python/Node present | Developer-environment docs referencing old commands — update `AGENTS.md`/contributing docs in the same commit |

## What stays

- `docs/` (current product docs + `zh/` translations) as the behavior reference until the Rust docs supersede page by page; `docs/design/` proposals remain clearly marked as proposals, never treated as ground truth (per AGENTS.md-adjacent guidance, history files do not describe current behavior).
- `docs-next/` itself — it becomes the rewrite's working record through P8.
- `LICENSE` (MIT, BigDawnGhost 2026) unchanged; BabelDOC AGPL isolation boundary respected throughout (no AGPL code enters the tree at any phase).
- Full git history — the deleted Python/TS implementation remains reachable as the eternal comparison oracle via tags; no separate archive copy.
- Root `config.yaml` semantics (schema preserved per D4; the file itself moves to whatever home the Rust binary documents).

## Per-item execution template

For each row above: (a) cite the precondition evidence (phase acceptance record + commit), (b) delete, (c) run the verification step and paste its output into the removal record, (d) run the full offline suite. If any step fails, restore and re-plan — never leave the tree half-deleted.

## Sources

- `apps/web/`; `apps/desktop/frontend/`; `packages/ui/` (`routes/manifest.tsx`); `packages/shared-schema/`
- `apps/api/` (`wenyi_api/`, `db/schema.sql`, `tests/conftest.py`); `packages/backend/wenyi_backend/`; `deploy/`; `docker/`; `packages/core/`; `packages/cli/`; `apps/desktop/backend/`; `apps/desktop/src/`
- `.github/workflows/` (`tests.yml`, `desktop.yml`, `build.yml`); `AGENTS.md`; `LICENSE`

## Open questions

Updater continuity (Q4) and any deprecation notice for compose users are tracked in `16-decisions-and-open-questions.md`.
