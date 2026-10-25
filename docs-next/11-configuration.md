# Configuration

Current configuration surface, per-item disposition, and the target YAML + per-project-settings + keyring design (decision D4 in doc 16: config stays YAML).

## 1. Current surface (verified)

### 1.1 `config.yaml` (repo root, 65 lines)

Read in full. Sections: `language` (source/target), `llm` (preset only in the shipped default: `preset: deepseek`), `segment` (`max_tokens_per_batch: 1800`, `max_tokens_per_segment: 1200`), `pipeline` (~20 flags: review/polish toggles, concurrency levels, review-loop knobs, pdf backend), `honorific` (strategy), `paths` (`state_dir: state`), `output` (mono/bilingual/order/about/punctuation). Full defaults live in code, not in this file.

### 1.2 `_DEFAULT_CONFIG_YAML` (`packages/core/wenyi_core/config.py:22-88`)

The template written by `Config.create_default_file` (`config.py:353-363`). Mirrors `config.yaml` with `preset: deepseek`.

### 1.3 `Config` / `SegmentConfig` / `PipelineConfig` / `OutputConfig` (`config.py:91-200`)

- `SegmentConfig`: token budgets (`max_tokens_per_batch/max_tokens_per_segment`); `extra="forbid"`.
- `PipelineConfig`: `translation_mode` (standard/best_of_three, validator requires polish for best_of_three at `config.py:113-117`), all review-loop/autofix flags, concurrency levels, `pdf_backend` (mineru/babeldoc), `babeldoc_bridge_url/timeout/pages`. Ignores legacy `precision_concurrency` (`config.py:105-111`).
- `OutputConfig`: mono/bilingual/order/preserve-style/about/punctuation.
- `Config`: `source_lang/target_lang` + the four sections + `honorific_strategy` + `state_dir`; rejects unknown sections (`from_dict`, `config.py:373-400`); freezes language policy plans after source detection (`freeze_language_policies`, `config.py:261-268`).

### 1.4 `llm/configuration.py` (`ProviderConfig/ModelConfig/RouteConfig/QuotaConfig/BudgetConfig/LLMConfig`)

Connection envelope (kind/base_url/api_key_env/timeout/retries/concurrency/quota_group), named model profiles, tier map (must define exactly strong/cheap/fast, `configuration.py:138-139`), per-operation routes with fallbacks, quotas, budget (`max_requests/max_tokens/deadline_seconds`). Presets expand then validate the full graph (`expand_preset`/`validate_graph`).

The provider registry (`packages/core/wenyi_core/llm/registry.py:113-139`) defines exactly 10 provider kinds: deepseek, openai, openrouter, opencode-go, openai-compatible, orcarouter, ollama, vllm, gemini, fake. The Rust port implements the same 10 kinds with the same per-provider option semantics.

### 1.5 `.env.example` (37 lines)

API keys (`DEEPSEEK/OPENAI/OPENROUTER/GEMINI/ORCAROOTER_API_KEY`, `MINERU_API_KEY`), service URLs (`DATABASE_URL`, `REDIS_URL`, `DATA_DIR`), `WENYI_API_TOKEN`, CORS, Docker mirrors. Notably: keys are env-only in CLI/Web; Desktop additionally supports manual entry with OS-vault storage (`apps/desktop/backend/wenyi_desktop/local_credentials.py`), never plaintext.

### 1.6 Per-package manifests

- Python: uv workspace (`pyproject.toml`, members `packages/core, packages/cli, packages/backend, apps/api, apps/desktop/backend`); per-package `pyproject.toml` with `hatch-vcs` versions from Git tags (AGENTS.md).
- TS: pnpm workspace (`pnpm-workspace.yaml`: `apps/web`, `apps/desktop/frontend`, `packages/ui`, `packages/shared-schema`); `packages/shared-schema` generates `api.d.ts` from a live API via openapi-typescript (shared brief).
- Rust: `apps/desktop/Cargo.toml` (`wenyi-desktop`, MIT, `rust-version = "1.89"`) — Tauri shell only.

### 1.7 Settings/policy bindings

- Global settings: PG `application_settings(id=1, document, default_template, revision)` (`apps/api/wenyi_api/db/schema.sql:2-8`); validation rejects project-level pipeline fields (`packages/backend/wenyi_backend/global_settings.py:30-43`); per-project pipeline fields enumerated in `config_documents.py:PROJECT_PIPELINE_FIELDS`.
- Per-project: PG `projects.config JSONB` + `strategy JSONB` (`schema.sql:23-24`); Desktop `application_settings(document)` + per-project docs in `local.sqlite3` catalog (`local_backend.py:52-70`).
- i18n policy: `i18n/policy/registry.py:OPERATIONS` allowlist (prompt.language_rules, punctuation.zh_cn, docx.chinese_font, markup.japanese_ruby, export.language_metadata); `Config.language_policy()` resolves per phase/path/format/backend (`config.py:204-233`); `_policy_tasks` maps phases to operation groups (`config.py:235-259`).

## 2. Disposition: keep / update / consolidate / replace / remove-later

| Item | Verdict | Notes |
| --- | --- | --- |
| Config section semantics (language/segment/pipeline/output/honorific) | KEEP | Field semantics and YAML shape are the product; only the layering changes |
| `LLMConfig` graph (providers/models/tiers/routes/quotas/budget) | KEEP, port field-for-field | Routing correctness depends on it (`validate_graph`); do not redesign |
| Preset expansion | KEEP | Builtin presets must keep working; store expanded form in the catalog for auditability |
| Language-policy tasks/registry/operations | KEEP, embed as resources | Prompt templates + 16 language JSONs become `include_str!`/data (shared verdict) |
| `config.yaml` root file + `_DEFAULT_CONFIG_YAML` | KEEP + PORT | Rust config parses the same YAML schema (D4); defaults render byte-identical; no TOML migration |
| `.env` key names | KEEP names, REPLACE mechanism | Same `*_API_KEY` names resolved from keyring/env; `.env` file remains a dev-only fallback, never the documented path |
| Global vs. project settings split (documents + PROJECT_PIPELINE_FIELDS) | CONSOLIDATE | Same conceptual split (global defaults vs. per-project strategy + locked `translation_mode`), one implementation in SQLite instead of three (PG JSONB / catalog docs / YAML) |
| Per-package Python/TS manifests, Docker, Compose, openapi-typescript pipeline | REMOVE-LATER | Deleted with the Python/TS products per shared verdicts; Rust workspace has one `Cargo.toml` |
| Tauri shell (`apps/desktop/Cargo.toml`, tauri.conf.\*) | REPLACE (salvage patterns) | Keep single-instance/tray/dialog/updater/keyring behaviors as specs; delete the shell |

## 3. Target design

### 3.1 Files

```text
<platform-config-dir>/wenyi/          # e.g. ~/.config/wenyi on Linux
├── wenyi.yaml                        # workspace config (global defaults + providers; same schema as config.yaml)
├── keyring: org.wenyi.desktop        # ALL secrets (provider keys, bridge tokens); never in YAML
└── workspace/
    ├── catalog.sqlite                # project catalog + global settings + job ledger (§3.3)
    └── projects/<id>/
        └── project.sqlite             # per-project state (see 12-data-and-persistence.md)
```

`wenyi.yaml` sketch (same schema as `config.yaml`; `[llm]` table mirrors `LLMConfig` 1:1):

```yaml
workspace:
  path: ~/Books/Wenyi          # default workspace; --workspace overrides

defaults:                      # global defaults; projects may narrow, never widen cost
  source_lang: auto
  target_lang: zh
  translation_mode: standard   # locked at project creation (as today)
  review: true
  polish: true

segment:
  max_tokens_per_batch: 1800
  max_tokens_per_segment: 1200

output:
  mono: true
  bilingual: false

llm:                           # provider/model/tier/route/quotas/budget tables as in LLMConfig
  preset: deepseek
```

Layering: `wenyi.yaml [defaults]` → project creation snapshot (frozen into `catalog.sqlite`, including expanded preset + policy fingerprint) → per-run overrides only for safe flags (concurrency, no-review/no-polish toggles). `global_settings.validate_settings` semantics (project fields rejected at global level) are preserved as a `SettingsLayer` validator.

### 3.2 Secrets

`keyring` crate, service `org.wenyi.desktop` (same service name as today, `local_credentials.py:23`). Account naming: `provider:<provider-name>` for LLM keys, `service:mineru` / `service:babeldoc` for PDF backends. Resolution order: keyring → process env (`*_API_KEY`, same names as `.env.example`) → error with actionable message. Session-only fallback behaves as today: memory-only with explicit warning, never persisted. No plaintext secrets in YAML/SQLite/logs — enforced by a `SecretString`-style redacting type at the config boundary.

### 3.3 Catalog tables (global settings live here)

Covered fully in `12-data-and-persistence.md`; the settings-relevant rows: `settings(id=1, document YAML-equivalent JSON, default_template, revision)` — same optimistic-revision pattern as PG `application_settings` — plus `provider_refs` (which keyring account backs each named provider, no secret material).

## 4. Migration

- Fresh install: write `wenyi.yaml` from embedded defaults (equivalent of `create_default_file`); `catalog.sqlite` created on first run with `PRAGMA user_version` schema tracking.
- Legacy import: none at v1 per D6 (no legacy import); if ever added, a one-shot `wenyi import-config config.yaml` reusing the same YAML schema needs no format conversion. `migrate-config`/`migrate-usage` CLI commands (`model_commands.py:135-157`) inform any future importer report format but are not ported as commands.
- CLI surface itself (parse/translate/prepare/status/assemble/report/language-policy/languages, glossary list/conflicts/resolve, models list/explain/check/ping) is a UX spec for the GUI's command palette and headless flags, not a ported binary — see `04-feature-inventory.md` §2 (surface inventory) and `05-target-architecture.md` (CLI crate); this doc owns only config semantics.

## Sources

- `config.yaml` (full read), `.env.example` (full read)
- `packages/core/wenyi_core/config.py` (incl. `_DEFAULT_CONFIG_YAML`, all four section models, `from_dict`, `freeze_language_policies`)
- `packages/core/wenyi_core/llm/configuration.py` (all six models, preset expansion, graph validation)
- `packages/core/wenyi_core/i18n/policy/registry.py` (OPERATIONS allowlist)
- `packages/backend/wenyi_backend/global_settings.py`, `config_documents.py`
- `apps/api/wenyi_api/db/schema.sql:1-8` (application_settings)
- `apps/desktop/backend/wenyi_desktop/local_backend.py:52-70` (catalog tables), `local_credentials.py:1-67` (SERVICE, keyring allowlist)
- `apps/desktop/Cargo.toml`, `pnpm-workspace.yaml`, root `pyproject.toml` (workspace members)

## Open questions

See `16-decisions-and-open-questions.md` (YAML key naming freeze; headless-mode flag parity list; updater signing-key custody shared with doc 10).
