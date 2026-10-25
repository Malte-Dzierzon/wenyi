# Project Vision

## End-state

Wenyi becomes a single native Rust desktop application: one binary, local-first,
EPUB-first, able to translate book-length works (up to ~10k pages) on a Linux
desktop. The direction is stated in `README.md` (fork statement): "a native
desktop application with a Rust backend and native Rust desktop GUI", "one
native desktop app; no separate web application or browser-based frontend".

Concretely, the end-state is:

- **Native Rust desktop.** Both engine and GUI are Rust; no Python runtime, no
  browser WebView, no sidecar process. See doc 05 for the crate layout.
- **Local-first.** All project state lives in local SQLite; translation runs
  in-process; the only network traffic is LLM API calls (and optional
  MinerU/BabelDOC conversion calls). No account, no server, no sync.
- **EPUB-first.** EPUB round-trip fidelity (TOC, anchors, annotations, images,
  bilingual styling) is the highest-priority format behavior; other formats
  follow. Current EPUB semantics to preserve are in doc 09.
- **10k-page scale.** Batch checkpoints, resumability, and streaming progress
  must hold at book scale, not just short-story scale.
- **Wayland-first, openSUSE Tumbleweed-first.** Primary development and test
  target is Wayland on openSUSE Tumbleweed; X11/other distros are best-effort.
  The GUI must be a minimal Material-3-inspired design (shared verdict: no
  Electron/Tauri/WebView shell).

## Non-goals

- **No web product or server.** No browser frontend, no self-hosted multi-user
  mode, no `apps/web`, no `packages/ui` as a web stack (see doc 17).
- **No PostgreSQL / Redis / Arq / Docker / deploy tooling in the final
  product.** These exist only to serve the web platform (`apps/api`,
  `deploy/`, `docker/`); the desktop product uses SQLite only (see doc 02).
- **No CLI as a shipped product surface.** The CLI command surface is reused
  *as a spec* for headless/scripted use only if cheap; it is not a supported
  product (doc 17 decides).
- **No new features during the rewrite.** Parity first; improvements go through
  doc 16 after parity.

## Users and workflows

The user is a single reader/translator working with long-form texts (novels,
monographs, narrative nonfiction — `README.md`, "Current functionality"). Core
workflows, preserved from the current 13-screen UI (`packages/ui/src/routes/manifest.tsx`):

1. **Translate a book:** import source → configure models/languages → run
   whole-book pipeline (prescan, translate, polish, review) → export.
2. **Control terminology:** inspect/approve glossary terms and resolve
   conflicts mid- or post-translation.
3. **Review with evidence:** read reviewer issues anchored to segments, apply or
   reject autofixes, proofread paragraph-by-paragraph.
4. **Resume and re-export:** reopen a project months later, continue an
   interrupted run, export to another format without retranslating.
5. **Translate subtitles:** lightweight SRT path without glossary/polish/review
   (`AGENTS.md`: "SRT independent of book Orchestrator").

## Guiding principles

1. **Local-first and offline-capable.** Everything except model inference works
   without a network; keys never leave the machine except to the provider.
2. **One binary.** Single installable artifact; no sidecar, no runtime to
   install (extends today's desktop promise in `docs/desktop.md`: "you do not
   need to install Python or deploy a server").
3. **Boring architecture.** Flat modules, explicit dependencies, no frameworks
   beyond need; dependency direction `UI → engine → storage` only (doc 05).
4. **Parity before polish.** Every migration phase exits on a parity checklist
   (doc 04), not on new capability.
5. **Deterministic core, model at the edges.** Pure functions for segmentation,
   anchors, punctuation, term matching; LLM calls isolated behind a router
   interface (current split: `wenyi_core/markup/`, `postprocess/` vs
   `wenyi_core/llm/`).
6. **Stable identities.** Segment/chapter/anchor IDs survive reruns and
   re-exports (`AGENTS.md` state invariants).
7. **Atomic, resumable state.** Manifest-last commits, batch checkpoints,
   once-only usage accounting (`AGENTS.md` state invariants; doc 07).
8. **Evidence over assertion.** Review issues must cite segments; quality claims
   need golden-file or A/B evidence (doc 14).
9. **Secrets stay secret.** Keys from environment or OS keyring only; never
   plaintext in SQLite/YAML/JSON/logs (`AGENTS.md` configuration section).
10. **Delete to finish.** Web stack, Python product code, and Tauri shell are
    removed after parity — no dual-stack maintenance (doc 17).

## Success criteria

- A user can install one binary on openSUSE Tumbleweed (Wayland), import an
  EPUB, translate a full book with glossary + review, and export EPUB/DOCX —
  with no Python, no server, no browser involved.
- Parity checklist (doc 04) passes: pipeline stages, formats, glossary, review,
  provider set, and 13-screen workflows behave as audited (doc 03).
- Interrupted runs resume without rework or double-billing; exports are
  byte-stable snapshots.
- Test gates pass: unit + golden-file + GUI E2E + model-quality comparison on
  the reference text (doc 14).
- Repository contains only the Rust product plus plan docs; all Python/TS
  product code, Docker, and deploy tooling deleted (doc 17).

## Sources

- `README.md` (fork direction, current functionality, limitations)
- `AGENTS.md` (project positioning, state invariants, secrets rule)
- `docs/desktop.md` (single-artifact promise, workspace pages)
- `packages/ui/src/routes/manifest.tsx` (13-screen inventory)

## Open questions

See `16-decisions-and-open-questions.md`.
