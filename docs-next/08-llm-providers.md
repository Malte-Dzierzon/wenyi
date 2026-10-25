# 08 — LLM Providers

Current provider layer (verified against `packages/core/wenyi_core/llm/`) and the target Rust provider-trait design. Prompt/agent behavior is doc 06; engine stage wiring is doc 07.

## 1. Current provider inventory

Registry: `llm/registry.py` (`ProviderSpec`, `PROVIDERS`, `provider_spec()`). **Ten** provider kinds are registered — the "12" figure in the shared brief is wrong; re-verified by reading the registry. Correct inventory:

| Kind | File | Client class | Protocol |
|------|------|--------------|----------|
| `deepseek` | `llm/providers/deepseek.py` | `DeepSeekClient` | OpenAI-compatible (`OpenAICompatibleBaseClient`), key `DEEPSEEK_API_KEY` |
| `openai` | `llm/providers/openai.py` | `OpenAIClient` | OpenAI-compatible base, key `OPENAI_API_KEY` |
| `openrouter` | `llm/providers/openrouter.py` | `OpenRouterClient` | OpenAI-compatible base |
| `opencode-go` | `llm/providers/opencode_go.py` | `OpenCodeGoClient` | OpenAI-compatible base, custom auth/headers |
| `openai-compatible` | `llm/providers/openai_compatible.py` | `OpenAICompatibleClient` | Generic base-URL client (covers LM Studio and any OpenAI-style endpoint) |
| `orcarouter` | `llm/providers/orcarouter.py` | `OrcaRouterClient` | Reuses `OpenAICompatibleOptions` (`options_module="openai_compatible"`) |
| `ollama` | `llm/providers/ollama.py` | `OllamaClient` | Reuses `OpenAICompatibleOptions`; local default base URL (`llm/providers/ollama.py:9`) |
| `vllm` | `llm/providers/vllm.py` | `VLLMClient` | Reuses `OpenAICompatibleOptions`; local/self-hosted default base URL |
| `gemini` | `llm/providers/gemini.py` | `GeminiClient` | Native Gemini protocol (`convert_messages_to_gemini()`, `extract_gemini_usage()`); keys `GEMINI_API_KEY` → fallback `GOOGLE_API_KEY` (`get_api_key_from_env()`, `llm/providers/gemini.py:120`) |
| `fake` | `llm/providers/fake.py` | `FakeProvider` / `FakeClient` | Deterministic test double, no network |

Shared OpenAI-compatible machinery: `llm/providers/_openai_compatible.py` (`OpenAICompatibleBaseClient`, `base_request_kwargs()`, `deep_merge()`, `normalize_openai_usage()`). Per-provider request shaping: `build_request_kwargs()` in each of `deepseek.py`, `openai.py`, `openrouter.py`, `opencode_go.py`, `openai_compatible.py`; reasoning-body helper `_reasoning_body()` (`llm/providers/openai_compatible.py:35`). Adapter contract: `llm/transport.py` (`ProviderAdapter`, `ResolvedModel`, `RequestContext`, `ConnectionOptions`); output-limit negotiation `output_limit()` (`llm/transport.py:120`, overridden `llm/providers/_openai_compatible.py:114`).

## 2. Tier routing and operation registry

Tiers: `TIERS = ("strong", "cheap", "fast")` (`llm/operations.py:14`). Operations registry: `OPERATIONS = register_operations(...)` (`llm/operations.py:52`) with `OperationSpec` (`llm/operations.py:18`: `id`, `tier | inherits`, `output_tokens`, `workflows`, `flags`, `review`, `resumable_conversation`, `protocol_version`). Current operation→default-tier mapping:

- `cheap`: `language.detect`, `annotation.align`, `review.scan`
- `strong`: `analysis.style`, `translation.body`, `translation.title`, `polish.body`, `review.verify`, `review.arbitrate`, `review.fix`, `srt.translate`
- `fast`: `synopsis.chapter`, `synopsis.book`, `glossary.extract`, `glossary.align_history`
- inherited: `autofix.verify ← review.verify`, `autofix.fix ← review.fix`

Flag gating: `workflow_operations()` / `configured_operations()` (`llm/operations.py:179`) map pipeline flags (`polish`, `review`, `review_agent_loop`, `review_conflict_arbitration`, `review_fix_loop`, `review_autofix`, `book_understanding`, `annotation_alignment`, `language_auto`) to reachable operations per workflow (`prepare | translate | review | srt`).

Route compilation: `llm/routing.py` (`resolve_routes()`, `model_route()`, `ResolvedRoute`, `inference_snapshot()`). Explicit `llm.routes.<operation>` entries select a model profile or tier with fallbacks; inherited operations follow their parent; failover is rejected for resumable conversations (`llm/routing.py:resolve_routes()`). Validation entry: `LLMConfig.validate_graph()` (`llm/configuration.py:validate_graph()`); preset expansion defaults to `deepseek` when config is empty (`LLMConfig.expand_preset()`).

Execution: `RoutedLLMClient` (`llm/router.py:27`) routes registered operations through adapters with one shared usage ledger; construction `llm/factory.py:build_client()`; options guard `_check_options()` (`llm/registry.py:15`) keeps routing/credentials/protocol out of raw overrides.

CLI surface (`packages/cli/wenyi_cli/model_commands.py`): `models list` (effective routes table/JSON via `resolve_routes()`), `models explain --operation` (`require_operation()` + route describe), `models check --for` (local credential validation, no network), `models ping --for` (one short live request per profile via `probe_connections()`), `models migrate-config` / `models migrate-usage` (explicit migrations via `llm/migration.py:convert_config()`).

## 3. Retry, limits, cancellation, usage, credentials

- Retry/backoff: `llm/retrying.py` — error taxonomy (`EmptyResponseError`, `TruncatedResponseError`, `ProviderRequestError`, `safe_provider_error()`), classification (`retry_reason()`, `is_retryable_provider_error()`, `is_resumable_provider_interrupt()` covering retries + 402/quota/5xx), server-hint-first wait (`_retry_after_seconds()` parsing `Retry-After`/`retry-after-ms`, capped at `_MAX_WAIT_SECONDS`), else exponential backoff with jitter (`wait_for_provider_retry()`), decorator factory `provider_retry()` (`llm/retrying.py:359`), reporting `RetryReporter` (`llm/retrying.py:273`). Tenacity is a core dependency used at call sites.
- Limits/concurrency/cancellation: `llm/limits.py` (`RequestLimits`) — one shared limiter per invocation: cooperative cancel (`cancel()` → `RequestCancelled`), deadline budget (`deadline_seconds` → `RequestStopped`), max-concurrency per connection, sliding-window quota (`requests_per_minute`, `tokens_per_minute` via `QuotaConfig`), token reservation budget (`BudgetConfig.max_requests/max_tokens`); permits never held during backoff (`wait_for_retry()`). Config: `ProviderConfig.max_concurrency`, `quota_group` (`llm/configuration.py:14`).
- Usage/cost: `llm/usage.py` — `UsageSample`, `make_usage_sample()`, `UsageTracker.record()` (thread-safe ledger with `by_tier/by_stage/by_provider/by_model` views), `merge_usage_summaries()` / `usage_delta()`, append-once commit path `RunStore.prepare_usage_commit()` / `recover_usage()` (`pipeline/runstore.py:437`); usage normalization per provider (`normalize_openai_usage()`, `normalize_deepseek_usage()`, `extract_gemini_usage()`).
- Credentials: env-only, never plaintext in state (`ProviderConfig.api_key_env`, validated as env-var name in `llm/configuration.py`; `validate_credential()` + `CredentialRedactor` in `llm/credentials.py`). Desktop stores secrets in OS keyring (`apps/desktop/backend` `CredentialStore`); web/desktop never persist keys in project state.
- Connectivity: `llm/connectivity.py` (`probe_targets()`, `probe_connections()`, `probe_profiles()`, `ProbeResult.describe()`); transport errors surfaced as `ProviderFailure` (`llm/errors.py:19`, `describe_provider_failure()`).

## 4. Model roles today vs target roles

Today roles are implicit in the operation→tier mapping (§2): strong = quality work (body, titles, polish, style analysis, evidence verify/arbitrate/fix, SRT), cheap = detection/annotation/scan, fast = synopsis/glossary. There is no per-role model beyond tier + optional per-operation route override.

Target roles (explicit, user-assignable, each defaulting to a tier but overridable to any configured model profile):

| Target role | Default tier | Covers (current operations) |
|-------------|--------------|------------------------------|
| `fast` | fast | `synopsis.*`, `glossary.*`, `language.detect` |
| `quality` | strong | `translation.body`, `translation.title`, `srt.translate` |
| `polish` | strong | `polish.body` |
| `analysis` | strong | `analysis.style` |
| `review` | cheap/strong mix | `review.scan` (cheap default), `review.verify/arbitrate/fix`, `autofix.*` (strong default) |
| (annotation stays) | cheap | `annotation.align` |

## 5. Target provider-trait design

- **No hardcoded providers/models**: a `Provider` trait (HTTP request build → response parse → usage sample) plus a registry mapping `kind → Provider impl`. Built-ins ship as registry entries (`openai-compatible` generic, `openai`, `deepseek`, `gemini`, `ollama`, `openrouter`, `orcarouter`, `opencode-go`, `vllm`, `fake` for tests) — same set, but adding a provider = new entry + options struct, never a core-code change. Model names are always user config, never constants.
- **OpenAI-compatible first**: the generic client (base URL + key + extra JSON options, reasoning-body passthrough, usage normalization) covers LM Studio, Ollama, vLLM, and any OpenAI-style gateway. Gemini keeps a dedicated protocol adapter (message conversion + usage extraction ported from `llm/providers/gemini.py`).
- **Per-model context limits**: each model profile declares `max_output_tokens` + context window; the router negotiates output limits per operation hint (port of `output_limit()`); batch sizing reads the active profile's limits instead of a global constant.
- **Rate limit / retry / timeout / concurrency**: port `RequestLimits` semantics (per-connection concurrency, quota windows, token reservation, deadline, cooperative cancel), server-hint-first retry with capped backoff (`Retry-After`/`retry-after-ms`), per-provider `timeout`/`max_retries`, resumable-interrupt classification (retryable + 402/quota/5xx). No permits held during backoff.
- **Token/cost tracking**: port the `UsageTracker` ledger views (`by_tier/by_stage/by_provider/by_model`) with append-once persistence and crash recovery; cost computation stays a presentation-layer multiply over recorded tokens (no pricing tables in the engine).
- **Credentials**: env-var indirection in config + OS-keyring storage on desktop; redaction in logs/events (port `CredentialRedactor`); `models check` equivalent validates locally without network.

### Migration notes per provider

- `openai-compatible` → the generic trait impl; first-class citizen for LM Studio/Ollama-style endpoints.
- `ollama` / `vllm` → thin presets over the generic impl (default base URLs + options structs ported).
- `openai` / `deepseek` → keep dedicated kinds for their request-shaping/usage quirks (`build_request_kwargs()`, usage normalizers).
- `openrouter` / `orcarouter` / `opencode-go` → gateway presets over the generic impl with their header/auth handling.
- `gemini` → dedicated protocol adapter (only non-OpenAI wire protocol).
- `fake` → deterministic test double in the Rust test harness.

### Acceptance criteria

- [ ] All 10 current provider kinds have a Rust registry entry with protocol parity (request-shape + usage-normalization golden tests against recorded fixtures).
- [ ] Tier→operation default map and flag gating reproduce `configured_operations()` for all four workflows on the shared config corpus.
- [ ] Retry classification + backoff + quota/concurrency/cancel behavior covered by unit tests (no sleeps; injected clock).
- [ ] `models list/explain/check/ping` equivalents exist in the Rust CLI surface (spec: doc 04 §2; crates: doc 05).
- [ ] No provider name, model name, or endpoint literal outside registry entries and tests.

## Sources

`packages/core/wenyi_core/llm/registry.py`, `operations.py`, `routing.py`, `router.py`, `configuration.py`, `retrying.py`, `limits.py`, `usage.py`, `credentials.py`, `connectivity.py`, `transport.py`, `errors.py`, `factory.py`, `migration.py`, `providers/_openai_compatible.py`, `providers/deepseek.py`, `providers/openai.py`, `providers/openrouter.py`, `providers/opencode_go.py`, `providers/openai_compatible.py`, `providers/orcarouter.py`, `providers/ollama.py`, `providers/vllm.py`, `providers/gemini.py`, `providers/fake.py`, `packages/cli/wenyi_cli/model_commands.py`, `packages/core/wenyi_core/pipeline/runstore.py`, `packages/core/wenyi_core/config.py`.

## Open questions

See doc 16 (`16-decisions-and-open-questions.md`) for the consolidated list.
