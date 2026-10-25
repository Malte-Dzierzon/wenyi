# Removal Plan

What gets deleted, in what order, and what must be proven first. Iron rule: **nothing is removed before it is migrated and validated** — each item's precondition names the parity proof, and the verification step makes the deletion checkable.

## Order (dependency-safe)

| # | Component | Precondition (parity proof) | Verification | Risk |
|---|---|---|---|---|
| CUT-1 | `docker/` + `deploy/` + nginx config + `.dockerignore` | None — proven web-only now (doc 03 infra table): no refs from desktop code/docs/CI/scripts, no CI image builds, no registry publishing | `docker/`, `deploy/` gone; `test_nginx_proxy.py` + Dockerfile param in `test_release_packaging.py:291` removed in the same change; offline suite green | Only external compose users — announce deprecation |
| CUT-2 | `apps/api` (+ PG schema, arq workers, `apps/api/tests/`) | CUT-1; `grep wenyi_api apps/desktop/backend packages/cli` empty (verified) | `grep -ri 'wenyi_api\|arq\|psycopg' --include='*.py' packages/ apps/desktop` empty; CI PG/Redis services + `pytest -m integration` PG tests removed; desktop suite green | None in-repo: `packages/backend` stays (desktop's largest dep — row CUT-2 explicitly keeps it) |
| CUT-3 | `apps/web` (+ Playwright tests/config) | CUT-2; U6 parity validation green (native UI covers the 13-screen inventory) | `git grep '@wenyi/ui'` shows only desktop frontend; web E2E retired with sign-off | Dropping a screen nobody ported — doc 18 §6 checklist per screen |
| 2 | `packages/ui` + `packages/shared-schema` + `apps/desktop/frontend` | CUT-3 + U6: no remaining TS consumer of `@wenyi/*` | `grep from '@wenyi/` empty; `pnpm` workspace has no members left | Shared validation logic hiding in TS — audit `lib/` for rules without a Rust counterpart before deleting |
| 5 | Python product code: `packages/core/wenyi_core`, `packages/cli`, `packages/backend`, `apps/desktop/backend` (+ all Python tests) | P2–P7 acceptance records complete: golden corpora, resume tests, CLI parity diff empty, eval comparisons recorded | `find . -name '*.py' -not -path './docs/*'` returns only tooling explicitly kept; `uv` workspace removed; full `cargo test` green with no Python toolchain installed | Oracle loss — keep the last Python tag reachable in git history (no separate archive needed; history is the archive) |
| 6 | Tauri shell: `apps/desktop/src/`, `apps/desktop/frontend` (if still present), tauri configs | U5 platform tests (single-instance, tray, dialogs, updater, keyring) green on the native GUI | `grep -ri 'tauri'` outside docs empty; no `*_tauri*` configs; release workflow builds only the native binary | Updater-channel continuity for existing installs — Q4 decision must land before this step |
| 7 | Toolchains: `uv.lock`, `pyproject.toml`s, `pnpm-workspace.yaml`, `package.json`s, Node/Python CI jobs | CUT-1–3 + 2 + 5–6 done and the tree builds + tests green with only `cargo` | Fresh-container check: clone, `cargo build`, `cargo test` (offline) with no Python/Node present | Developer-environment docs referencing old commands — update `AGENTS.md`/contributing docs in the same commit |

## What stays

- `docs/` (current product docs + `zh/` translations) as the behavior reference until the Rust docs supersede page by page; `docs/design/` proposals remain clearly marked as proposals, never treated as ground truth (per AGENTS.md-adjacent guidance, history files do not describe current behavior).
- `docs-next/` itself — it becomes the rewrite's working record through P8.
- `LICENSE` (MIT, BigDawnGhost 2026) unchanged; BabelDOC AGPL isolation boundary respected throughout (no AGPL code enters the tree at any phase).
- Full git history — the deleted Python/TS implementation remains reachable as the eternal comparison oracle via tags; no separate archive copy.
- Root `config.yaml` semantics (schema preserved per D4; the file itself moves to whatever home the Rust binary documents).

## Per-item execution template

For each row above: (a) cite the precondition evidence (phase acceptance record + commit), (b) delete, (c) run the verification step and paste its output into the removal record, (d) run the full offline suite. If any step fails, restore and re-plan — never leave the tree half-deleted.

## Deferred deletions (evaluated 2026-10-25, do not pull forward)

| Candidate | Why deferred |
|---|---|
| Buildx variants (`Dockerfile.api.buildx`, `Dockerfile.web.buildx`, `docker-compose.buildx.yml`) | Only 3 files; deleting now also touches `test_release_packaging.py:291` (parametrized over both) + `docs/web.md` + `docs/zh/web.md`. Dies with row 4 anyway. |
| `test_nginx_proxy.py` | Always skipped without `WENYI_TEST_DOCKER=1` (never set in CI); harmless. Dies with row 4. |
| CI `dev`/`webui` branch triggers (`tests.yml`, `build.yml`) | Never fire on this fork (only `main` exists); cosmetic. Rewritten with Rust CI at row 7. |
| `.agents/skills/land` | Targets upstream `dev` + `BigDawnGhost/wenyi`; update branch/repo refs when the fork's landing flow is defined, not now. |
| `docs/design/` proposals | Already marked non-behavioral; deleting history has no maintenance benefit. Keep. |
## Sources

- `apps/web/`; `apps/desktop/frontend/`; `packages/ui/` (`routes/manifest.tsx`); `packages/shared-schema/`
- `apps/api/` (`wenyi_api/`, `db/schema.sql`, `tests/conftest.py`); `packages/backend/wenyi_backend/`; `deploy/`; `docker/`; `packages/core/`; `packages/cli/`; `apps/desktop/backend/`; `apps/desktop/src/`
- `.github/workflows/` (`tests.yml`, `desktop.yml`, `build.yml`); `AGENTS.md`; `LICENSE`

## Open questions

Updater continuity (Q4) and any deprecation notice for compose users are tracked in `16-decisions-and-open-questions.md`.
