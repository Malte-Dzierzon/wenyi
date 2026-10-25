# Project Vision

## End-state

Wenyi becomes a single native Rust desktop application: one binary, local-first,
EPUB-first, able to translate book-length works (up to ~10k pages) on a Linux
desktop (Wayland on openSUSE Tumbleweed first; X11/other distros best-effort).

- **Native Rust desktop.** Engine and GUI are Rust; no Python runtime, no
  browser WebView, no sidecar process. Crate layout: doc 05.
- **Local-first.** All project state in local SQLite; translation in-process;
  the only network traffic is LLM API calls (plus optional MinerU/BabelDOC
  conversion). No account, no server, no sync.
- **EPUB-first.** EPUB round-trip fidelity (TOC, anchors, annotations, images,
  bilingual styling) is the top format priority; other formats follow (doc 09).
- **Minimal GUI, UI-first.** Native UI rebuilt against the live Python backend
  first (translation, not redesign — doc 18); GPUI-Kit 0.8, decided by U0
  spike (doc 10 §3). Consolidation to 7 destinations happens only after
  parity.

## Non-goals

- **No web product or server.** No `apps/web`, no `apps/api`, no PG/Redis/arq,
  no `docker/`/`deploy/` (doc 17). These serve only the web platform (doc 02).
- **No new features during the rewrite.** Parity first; improvements go through
  doc 16 after parity.
- **No CLI as a shipped product.** The CLI surface is a spec for headless use
  (doc 04 §2); the GUI is the product.

## Core workflows (from `packages/ui/src/routes/manifest.tsx`)

1. **Translate a book:** import → configure models/languages → run pipeline
   (prescan, translate, polish, review) → export.
2. **Control terminology:** inspect glossary terms, resolve conflicts.
3. **Review with evidence:** read reviewer issues anchored to segments, apply or
   reject autofixes, proofread paragraph-by-paragraph.
4. **Resume and re-export:** reopen months later, continue interrupted runs,
   export to another format without retranslating.
5. **Translate subtitles:** lightweight SRT path, no glossary/polish/review.

## Principles

1. **Local-first.** Everything except model inference works offline; keys never
   leave the machine except to the provider.
2. **One binary.** No sidecar, no runtime to install.
3. **Boring architecture.** `UI → engine → storage` only (doc 05); no
   frameworks beyond need.
4. **Parity before polish.** Every phase exits on the doc 04 checklist.
5. **Deterministic core, model at the edges.** Pure functions for segmentation,
   anchors, punctuation, term matching; LLM calls behind a router interface.
6. **Stable identities.** Segment/chapter/anchor IDs survive reruns and
   re-exports (`AGENTS.md` state invariants).
7. **Atomic, resumable state.** Manifest-last commits, batch checkpoints,
   once-only usage accounting (doc 07).
8. **Secrets stay secret.** Keys from environment or OS keyring only; never
   plaintext in SQLite/YAML/JSON/logs.
9. **Delete to finish.** Web stack, Python product code, Tauri shell removed
   after parity — no dual-stack maintenance (doc 17).

## Success criteria

- One binary on Tumbleweed/Wayland: import EPUB → translate full book with
  glossary + review → export EPUB/DOCX. No Python, server, or browser.
- Doc 04 checklist passes; interrupted runs resume without rework or
  double-billing; exports are byte-stable snapshots.
- Test gates pass: unit + golden-file + GUI E2E + model-quality comparison
  (doc 14).
- Repo contains only the Rust product plus plan docs (doc 17).

## Sources

- `README.md`, `AGENTS.md`, `docs/desktop.md`
- `packages/ui/src/routes/manifest.tsx` (13-screen inventory)
