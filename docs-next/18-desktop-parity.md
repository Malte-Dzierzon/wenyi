# Desktop UI Parity Plan

Screen-by-screen reference for reproducing the existing desktop UI in the
native toolkit (GPUI-Kit 0.8, decided by U0 spike — doc 10 §3). Rule: **translation, not
redesign** — same information architecture, same workflows, same backend ops.
The 7-destination consolidation (doc 10 §4) is post-parity.

## 1. Current desktop stack (verified)

```text
Tauri 2 host (apps/desktop/src, 9 Rust modules)
  spawns/monitors → Python sidecar (wenyi_desktop: SQLite + local runner)
  loads WebView → React host (apps/desktop/frontend/src/main.tsx)
                    renders @wenyi/ui App (13 shared screens)
```

- Bootstrap: `main.tsx` waits for runtime readiness, configures
  `desktopPlatform(queryClient)`, mounts `App`.
- Discovery: `runtime.ts` reads `window.__WENYI_DESKTOP__` (`apiBase`, `token`)
  injected by the Tauri host after sidecar ready; states
  pending/ready/error/closing.
- Transport: `transport.ts` — REST `${apiBase()}${path}` + bearer token, WS
  `/ws/projects/{pid}/progress` (kinds `snapshot|stats|state|progress|batch|
  chapter|term|pipeline|log` + `final`), multipart upload for project create.
- Shared UI is host-neutral: `packages/ui/tests/platform-boundaries.test.mjs`
  forbids `localStorage`/`fetch`/`WebSocket` in `packages/ui/src`; desktop
  injects capabilities via `desktopPlatform` (native drop/export, credentials,
  updates, MinerU).
- Deleting `apps/web` does not affect the desktop UI: only `apps/web` and
  `apps/desktop/frontend` consume `@wenyi/ui`; shared screens gate optional

## 1b. Reconciled findings (5-agent audit 2026-10-25)

Agents A–E audited shell, project workflows, content workflows, settings, and
desktop integration against source. Reconciled results:

- **Route tree verified (Agent A):** 14 routeEntries (1 redirect
  `/settings/:section`, 1 catch-all in App.tsx) + 13 navEntries. Global nav:
  Projects (`LayoutDashboard`), Create project (`FolderPlus`), Interface
  settings (`Settings`). Project nav order: overview (`Sparkles`),
  proofreading (`BookOpenCheck`), review (`ListChecks`), subtitles
  (`Captions`, srt only), glossary (`Library`), style (`Languages`), contents
  (`ListTree`), export (`FileOutput`), settings (`Settings2`), events
  (`ScrollText`). Audience rule: book = fmt truthy and ≠ srt.
- **Dead UI surface (verified, do NOT port):** `POST …/prepare`,
  `POST …/assemble` (no `api.*` method, no UI caller);
  `POST …/review/{ci}/complete` (no UI caller); `/strategies/*` (no UI
  caller). Backend keeps them; the native UI must not invent callers.
- **Progress controls (Agent B):** header exposes translate/pause/resume
  only — not prepare/assemble. Accounting metrics are exactly: cumulative
  tokens, requests, cache hit rate, recorded time. ChapterTable review badge
  = `review_issue_count` + `review_status ∈ {completed,ok,done}`.
- **Glossary (Agent C):** no sorting (search q + type filter only);
  import/export CSV+JSON; import conflicts resolved client-side before POST.
- **Review run button (Agent C):** disabled unless `!historical && !busy &&
  !active && translated && !loading && !error`.
- **Style (Agent C):** readOnly = `busy || saving || !data`; path is
  `/analysis`, not `/style/analysis`.
- **Precision drafts (Agent C):** `GET …/chapters/{ci}/segments/{si}/
  precision-drafts` (chapters router); polls 3s only when busy +
  unavailable (`no_archive`/`incomplete`).
- **Settings (Agent D):** project `llm` restricted to tiers/routes/budget;
  global is full registry + workflow + MinerU; credentials via desktop REST +
  keyring with pending-write safety; model list via
  `POST /settings/models/list`; ping via `POST /settings/models/ping`.
- **WS (Agent E):** `final` is a UI-side alias; backend emits `state`.
  Desktop `progressKeys` map verified (`platform.ts:29-41`).
- **Locks (Agent E, Phase 6 input):** one sidecar per workspace
  (`.desktop.lock` + `.desktop-instance.lock`). Two UIs CANNOT each spawn;
  but both CAN attach to one sidecar — the comparison story is
  Tauri-spawns + native-attaches (or vice versa), never two spawns.
- **Storage (Agent E, Phase 6 input):** `<workspace>/local.sqlite3`
  (catalog) + `projects/<pid>/state.sqlite3` (+ source/, reviews/, lock
  files). Project id = 16 hex chars; initialized ⇔ state.sqlite3 loads.
  No import/migration work in this phase; project data untouched.

## 2. Screen inventory (all 13)

Source of truth: `packages/ui/src/routes/manifest.tsx` (`routeEntries :79`,
`navEntries :125`, `isVisibleFor :103`). Feature code:
`packages/ui/src/features/`. Backend ops via `packages/ui/src/lib/api.ts`
(52 `request<` call sites) + `lib/ws.ts`.

| # | Screen | Route | Source | Backend ops | States | Status |
|---|---|---|---|---|---|---|
| 1 | Dashboard | `/` | `features/dashboard/Dashboard.tsx` | `listProjects`, `deleteProject` (confirm) | loading, empty, error, busy-delete | Functionally verified (live fetch + refetch + delete + card click-through; dark + pixel-compare pending) |
| 2 | Create project | `/projects/new` (+ `?project=` resume-edit) | `features/project-create/CreateProject.tsx` + SourceFilePicker, SourcePreview | `capabilities`, `getProject`, `getConfig`, `getPreview`, `createProject` (multipart), `resume`, `translate` | loading, parse-preview, busy, error, locked-fields | Implemented (form + rfd browse + validation + multipart + preview poll; body shape verified vs live backend; resume-edit path + human file-pick test pending) |
| 3 | Progress / overview | `/projects/:pid` | `features/progress/ProgressPage.tsx` + WorkflowPanel, Accounting, charts | `getProject`, `getWorkflow`, `listChapters`, `getSubtitles`, `getReport`, `regenerateReport`, `getStats`, `translateChapter`, run controls (`translate`/`pause`/`resume`); WS progress | loading, busy, paused, error, empty-report, not-initialized | Implemented (run controls + overview + chapters + report + stages; serde verified vs live backend; WS live-updates + charts + human click test pending) |
| 4 | Glossary | `/projects/:pid/glossary` (book only) | `features/glossary/GlossaryPage.tsx` | `listTerms` (q+type filter, no sort), `addTerm`, `updateTerm`, `deleteTerm` (batch), `listConflicts`, `resolveConflict`, `importGlossary` (CSV/JSON), `downloadGlossary` (CSV/JSON) | loading, empty, busy, conflicts, import steps | Implemented (list + search + type chips + select/batch + conflicts + add/edit/delete wired; serde verified vs live backend; dialog field inputs + import/export file flows + human test pending) |
| 5 | Style | `/projects/:pid/style` (book only) | `features/style/StylePage.tsx` | `getAnalysis`, `updateAnalysis` (7 fields + guide + synopsis), `updateDigest` per chapter | loading, busy, read-only, edit states | Implemented (tabs + 7 fields + guide + synopsis + digest list/edit; 500-before-prepare handled as empty form; human test pending) |
| 6 | Contents | `/projects/:pid/contents` (book only) | `features/contents/ContentsPage.tsx` + ContentsList/Editor | `listChapters` (client search), `updateChapterTitle` (modal, stale-reload) | loading, empty, busy, read-only, stale | Implemented (list + client search + edit dialog with stale guard + empty guard; 409-before-prepare verified vs live backend; human test pending) |
| 7 | Review | `/projects/:pid/review` (book only) | `features/review/ReviewPage.tsx` + ReviewActivity/Issues/ItemDetails | `getProject`, `listChapters`, `getWorkflow`, `listReviewRuns`, `getReviewRun`, `runAiReview`; WS progress | loading, empty, busy, historical, issue-filter | Implemented (history select + gated run button + activity + issues search/filter/counts/expand + evidence; empty-runs + 409-gate verified vs live backend; WS live progress + proofreading links + human test pending) |
| 8 | Proofreading | `/projects/:pid/proofreading[/:ci]` | `features/proofreading/` (Page, ChapterProofreading, ParagraphEditor, PrecisionDrafts, RevisionHistory, ParagraphActions) | `listChapters`, `getReview`, `segmentHistory`, `precisionDrafts` (conditional poll), `editSegment` | loading, empty, busy, read-only, stale, context-menu | Implemented (chapter list + rows + editor tabs edit/history/precision + save with stale guard; `ChapterSegments` serde verified vs live backend; 409-before-prepare handled; modal semantics + context menu + deep-link + diff + human test pending) |
| 9 | Project settings | `/projects/:pid/settings` | `features/settings/SettingsPage.tsx` | `getProject` (busy gate), `getConfig`, `capabilities`, `modelRoutes`, `saveConfig`, `validateConfig`, `getProjectDefaults`, `checkModels` | loading, invalid, busy, unsaved-YAML | Implemented by sub-agent (tier display + workflow form + YAML editor + routes tables + restore/validate/save/check; per-op model overrides omitted — needs ops list; human + live test pending) |
| 10 | Subtitles | `/projects/:pid/subtitles` (srt only) | `features/subtitles/SubtitlesPage.tsx` | `getProject`, `getSubtitles`, `editSubtitle`; client search + 50/page pagination | loading, empty, empty-search, busy | Implemented (count + search + cue cards + edit + pagination; `SubtitleList` serde verified vs live backend incl. 404-before-prepare; human test pending) |
| 11 | Export | `/projects/:pid/export` | `features/export/ExportPage.tsx` | `getConfig`, `getProject`, `capabilities`, `listExports` (2s poll), `createExport` or native `saveExport`, `downloadExport` or native save | loading, pending/running, error, empty | Implemented by sub-agent (format grid + edition + advanced + table + native plan→rfd→content→write flow; toasts/disclosure-dates as inline; human + live test pending) |
| 12 | Events | `/projects/:pid/events` | `features/events/EventsPage.tsx` | `listEvents` (no UI filter; 5s refetch; desc) | loading, empty, error | Implemented by sub-agent (rows + tone badges + expandable payload + 5s poll + desc sort; raw ISO timestamps; human + live test pending) |
| 13 | Interface settings | `/settings` | `features/settings/InterfaceSettingsPage.tsx` → `GlobalConfiguration.tsx` | locale prefs + full global config: `getGlobalConfig`, `saveGlobalConfig`, `validateGlobalConfig`, `getGlobalDefaults`, `capabilities`, `pingModels`, `listModels`, credentials, MinerU, updater | normal, pending, invalid, blocked | Implemented by sub-agent (registry + workflow + MinerU + YAML + ping + credentials + revision handling; updater placeholder; rename-maps need Client extension; human + live test pending) |

Status vocabulary: Not audited / Audited / Mapped / Implemented /
Functionally verified / Visually compared / Complete / Blocked. All 13 screens
are Audited + Mapped + Implemented as of the parallel build (lead: screens
1–8, 10; sub-agents: 9, 11–13). Screen 1 is Functionally verified; the rest
need human interaction testing + pixel comparison before advancing.

Shell elements (all screens): Brand link, sidebar collapse toggle (persisted),
global nav (Projects/Create/Interface settings, hover prefetch),
project nav (audience-filtered), NotificationToaster, RouteGate
(suspense + per-route error boundary), AppErrorBoundary. No in-app dark-mode
toggle (host/CSS driven). Route count: 14 routeEntries (1 redirect
`/settings/:section`), 13 navEntries.

Desktop-only UI (not in shared routes): credential fields
(`DesktopCredential.tsx`, `/desktop/credentials…`), MinerU credential
(`MinerUCredential.tsx`, `/desktop/external-credentials/mineru…`), updater +
release notes (`DesktopUpdates.tsx`, `ReleaseNotes.tsx`, Tauri
`desktop_update_*` commands), native drop (`nativeDrop.ts`,
`native_drop_release/upload`), native save (`nativeExport.ts`,
`native_export_save`), update safety gating (`updateSafety.ts`).

Shared components (`packages/ui/src/components` + feature-local): `AppLayout`,
`Navigation`, `PageHeader`, `PageContainer`, `Brand`, `NotificationToaster`,
`LazyBoundary`, `RouteGate`, `AppErrorBoundary`; primitives (button, card,
badge, inputs, select, disclosure, progress, dialogs); `SegmentEditor`,
`StatusBadge`, `WorkflowPanel`, `Accounting`, charts, pickers, editors.

Styling: Tailwind + `tailwind.preset.js` (px scale, typography, shadcn-style
HSL tokens in `index.css`, light/dark via `.dark` class, no in-app toggle),
brand font, custom focus/geometry CSS. i18n: `en` + `zh-CN`
(`packages/ui/src/i18n/locales/`) + desktop keys (`useDesktopI18n`).

## 3. Minimal backend op list for the UI

The native UI reuses the loopback contract (no new protocol). Routers:
14 shared (`packages/backend/wenyi_backend/routers/`) + 4 desktop-only
(`apps/desktop/backend/wenyi_desktop/routers/` + `updates.py`).

Essential ops by workflow (prefixes verified):

- Projects: `GET/POST /projects`, `GET /projects/{pid}`,
  `POST /projects/{pid}/upload`, `GET …/preview`,
  `POST …/translate|prepare|assemble|pause|resume`, `DELETE /projects/{pid}`
- Config/models: `GET …/config|config/defaults|models|stats|workflow`,
  `PUT …/config`, `POST …/config/validate|models/check`
  (`configuration.py`), `GET/PUT /settings…`, `POST /settings/models/ping`
- Chapters: `GET /projects/{pid}/chapters[/{ci}]`,
  `PUT …/{ci}/title`, `POST …/{ci}/translate`
- Glossary: `GET/POST /projects/{pid}/glossary/terms`,
  `PUT/DELETE …/terms/{source}`, `GET …/conflicts`,
  `POST …/conflicts/{cid}/resolve`, `GET …/export`, `POST …/import`
- Review/proofreading: `POST …/review/run`, `GET …/review/runs[/{rid}]`,
  `GET …/review/{ci}`, `PUT …/{ci}/segments/{seg_idx}`,
  `GET …/history`, `POST …/{ci}/complete`
- Style: `GET/PUT …/style/analysis`, `PUT …/chapter-digests/{ci}`
- Subtitles: `GET/PUT …/subtitles[/{cueid}]`
- Export: `GET/POST …/exports`, `GET …/exports/{id}/download`; desktop-native:
  `POST /desktop/projects/{pid}/exports/plan`, `GET …/{id}/plan|content`
- Events/report: `GET …/events`, `GET/POST …/report`
- Credentials: `GET /desktop/credentials[/{connection}/secret]`,
  `GET /desktop/external-credentials/mineru[/secret]`
- Progress: WS `/ws/projects/{pid}/progress` (first frame `{token}`)
- Health: `GET /health` (unauthenticated)

## 4. GPUI-Kit component mapping (API-verified 2026-10-25)

Sources: `~/.cargo/registry/.../gpui-component-0.8.0`,
`gpui-base-0.8.0`. Fit: exact / close / custom.

| Wenyi element | Source | GPUI-Kit / primitive | Fit | Notes |
|---|---|---|---|---|
| Sidebar + collapse toggle | `AppLayout`, `Navigation` | `Sidebar::new` + `SidebarToggleButton`, `.collapsible` | exact | `sidebar/mod.rs:222`; persist collapsed pref locally |
| Buttons (default/destructive/outline/secondary/ghost/link; sm/md/lg/icon) | `ui/button.tsx` (cva) | `Button::new` + `ButtonVariants` (`.primary()` etc.) | exact | U0-verified click handling |
| Brand link + emblem | `Brand` | `div` + asset image + text | close | reuse `wenyi-emblem.png`/wordmark SVG via `Assets` |
| Cards (dashboard, overview) | `ui/card.tsx` | `div`-based card / component `Card` | close | rounded-lg border shadow-sm p-6 → style constants |
| Badges (7 variants) | `ui/badge.tsx`, `StatusBadge` | `Badge` / label primitives | exact | map `statusTone` → variant |
| Text inputs, search fields | `ui/form.tsx`, search inputs | `Input::new(&Entity<InputState>)` | exact | U0-verified set/readback; IME via `zwp_text_input_v3` |
| Password inputs (credentials) | `ui/password-input.tsx` | `Input` + visibility toggle | close | show/hide labels; memory-only drafts |
| Textareas (YAML, guide, synopsis) | `Textarea` | `TextareaState` / `Editor` w/o LSP | close | mono + spellcheck-off attrs |
| Selects (langs, PDF engine, history) | `ui/select.tsx` | `Select::new(&Entity<SelectState<D>>)` | exact | `select.rs:138`; delegate per option list |
| Checkboxes, radios | native inputs | `Checkbox::new`, `RadioGroup` | exact | 16px geometry → style constants |
| Disclosure/collapsible | `ui/disclosure.tsx` | `Collapsible` | exact | chevron rotate behavior |
| Progress bars | `ui/progress.tsx`, `ReviewActivity` | `Progress::new().value(0..100)` | exact | `progress.rs:13`; circle variant for compact |
| Dialogs (add/edit/import/confirm/ParagraphEditor) | `Dialog`, native `<dialog>` | `Dialog::new` (`.title/.content/.footer/.on_ok/.on_cancel`) / `AlertDialog` | close | focus restore + autofocus ported manually; ParagraphEditor tabs = `TabBar` inside dialog |
| Tabs (Style, ParagraphEditor, settings) | `Tabs` | `TabBar::new` + `Tab::new` | exact | arrow/home/end keyboard ported (verified API `tab_bar.rs:48`) |
| Glossary/terms/events tables | tables | `DataTable::new(&Entity<TableState<D>>)` + `Column::new` | exact | checkbox select, batch bar; no sort needed (source has none) |
| Chapter/contents lists | lists | `v_virtual_list` + `VirtualListScrollHandle` | exact | `scroll_to_item` for `?segment=` deep-link |
| Chapter tables (progress) | `ChapterTable` | `DataTable` (read-mostly) | exact | per-row translate buttons |
| Context menu (paragraphs) | `ParagraphActions` (role=menu) | GPUI context menu / custom popup | custom (small) | arrow nav, Esc/Tab close, Shift+F10 trigger — port semantics |
| Toasts | `NotificationToaster` (sonner) | `Notification::info/success/warning/error` + `NotificationList` | exact | `notification.rs:114` |
| Release notes / report markdown | `Markdown` (react-markdown+gfm) | `TextView::markdown` | exact | links/images neutralized as today |
| Review evidence StructuredData | `StructuredData` | `TextView` / tree | close | nested data rendering |
| Events expandable rows | `<details>` | `Collapsible` per row | exact | timestamp + Badge + payload |
| Review run detail + side panels | splits | `DockArea` or plain splits | close | serializable layout likely unneeded — prefer splits first |
| Charts (Accounting) | `RunTimeChart`, `UsageChart` | `chart/*` module (unstyled) or custom draw | custom (medium) | area/bar/line exist but not shadcn-styled; verify look in U2 |
| YAML config editor | `Textarea` + validate/save | `Editor` w/o LSP | close | small: reuse Editor, no tree-sitter needed |
| WorkflowPanel/Accounting composites | custom | Form + Table composites | custom (medium) | build from primitives |
| Precision drafts + diff | `PrecisionDrafts`, `TranslationDiff` | `Editor`/`TextView` + custom diff | custom (medium) | retry + polling semantics ported |
| File drop zone | `SourceFilePicker` + `nativeDrop.ts` | GPUI drag events + `native_drop_*` port | custom (medium) | drop-into-window unverified — U1 risk |
| File browse / native save | buttons + `nativeExport.ts` | `rfd` (`pick_file`/`save_file`, off UI thread) | close | port `native_export.rs` atomic-publish + update gating |
| Updater + confirm dialog | `DesktopUpdates` | `Dialog` + custom signed-manifest client | custom (medium) | port `updates.rs` phases; Q4 before U5 |
| Tray / single-instance | Tauri shell | `tray-icon` + `single-instance` crates | custom (medium/large) | GPUI event-loop interop unverified — U5 risk |
| Keyring credentials | `CredentialStore` | `keyring` crate (off-thread) | close | service `org.wenyi.desktop`; memory-only fallback |
| Locale switch (en/zh-CN) | `LanguageSettings` | `Select` + local pref store | exact | ~600 keys, `{word}` interpolation, no plurals — port catalog as data |
| Pagination (subtitles 50/page) | buttons | Button pair + state | exact | trivial |
| Search filters (all lists) | `Input` | `Input` + client filter | exact | all filtering is client-side today |

Theming: Wenyi HSL tokens (`index.css:18-64`) map 1:1 to GPUI-Kit
`ColorTokens` (`theme_tokens.rs:20`, `.light()`/`.dark()`); `--radius: 8px`
= `RadiusTokens.lg`. Full px spacing/typography scales reproduced as Rust
style constants (theme has only 7 spacing + 6 radius tokens). Light/dark via
`Theme::change`. Brand font `DreamHanWenyi` woff2 reused as asset.

## 5. Implementation order

1. Shell + Library (screens 1–2): window, nav, project list, create flow
   (upload + preview). Proves transport + discovery + multipart.
2. Overview + progress (screen 3): run control + WS progress + report.
   Proves the event taxonomy end to end.
3. Read & correct path (screens 6 + 8): chapter list + segment editor.
   Proves the Editor component + edit ops + history.
4. Glossary (screen 4): tables + dialogs + import/export. Proves DataTable.
5. Review (screen 7) + Style (5): runs + evidence + synopsis editing.
6. Export (11) + native save + credentials + updater (desktop-only layer).
7. Settings (9 + 13) + Subtitles (10) + Events (12). Subtitles is
   conditional (srt only); Events is the future drawer.

Each step ships behind the existing Tauri app as fallback; the Tauri app
is removed only after full parity validation (doc 17 row 6).

## 6. Parity validation (per screen, before redesign)

- [ ] Same backend ops called with same payloads (trace vs React app).
- [ ] Same loading/error/empty states reachable (drive each state).
- [ ] Side-by-side screenshot comparison (light + dark, en + zh-CN).
- [ ] Progress/WS behavior matches at 4 Hz coarse + targeted invalidation.
- [ ] No translation-behavior change: export bytes identical for the
  fixture book through old and new UI (doc 14 §4 U-phase strategy).
- [ ] Existing desktop E2E suite still passes against the Tauri fallback.

## Sources

- `apps/desktop/frontend/src/` (all 17 files)
- `packages/ui/src/routes/manifest.tsx`, `lib/api.ts`, `lib/ws.ts`,
  `platform.ts`, `features/`, `components/`, `index.css`
- `packages/backend/wenyi_backend/routers/`, `application.py`
- `apps/desktop/backend/wenyi_desktop/routers/`, `updates.py`, `main.py`
- `apps/desktop/src/` (Tauri commands)
- `https://gpui-kit.com/component` (catalog), GPUI-Kit task/test/a11y docs
