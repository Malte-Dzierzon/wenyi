//! Provider adapters. Oracle: `llm/providers/*` + `llm/registry.py`.
//!
//! Nine OpenAI-compatible kinds share one wire core (request bodies,
//! response handling, usage normalization); Gemini speaks its own REST
//! dialect; `fake` is the deterministic offline provider. Provider quirks
//! stay in this module — the router only sees `ProviderKind`.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::config::{ModelConfig, ProviderConfig};
use crate::{Error, Result};

/// Reserved model-option keys (oracle `_check_options` protected set).
/// Routing, credentials, and protocol ownership must never hide in raw
/// request overrides.
const PROTECTED_OPTIONS: [&str; 24] = [
    "apikey",
    "apitoken",
    "accesstoken",
    "authorization",
    "headers",
    "extraheaders",
    "cookies",
    "model",
    "messages",
    "contents",
    "systeminstruction",
    "stream",
    "maxtokens",
    "maxcompletiontokens",
    "maxoutputtokens",
    "responseformat",
    "responsemimetype",
    "baseurl",
    "httpoptions",
    "clientargs",
    "retryoptions",
    "maxretries",
    "timeout",
    "apikey",
];

/// All provider kinds (oracle `PROVIDERS` registry keys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// DeepSeek native endpoint.
    Deepseek,
    /// Official OpenAI endpoint.
    Openai,
    /// OpenRouter gateway.
    Openrouter,
    /// OpenCode Go gateway.
    OpencodeGo,
    /// Arbitrary Chat Completions endpoint.
    OpenaiCompatible,
    /// OrcaRouter gateway.
    Orcarouter,
    /// Local Ollama daemon.
    Ollama,
    /// Local vLLM server.
    Vllm,
    /// Local llama.cpp server.
    Llamacpp,
    /// Google Gemini REST API.
    Gemini,
    /// Deterministic offline provider.
    Fake,
}

impl ProviderKind {
    /// Parse a registry kind, rejecting unknown values explicitly.
    pub fn require(kind: &str) -> Result<Self> {
        Ok(match kind {
            "deepseek" => Self::Deepseek,
            "openai" => Self::Openai,
            "openrouter" => Self::Openrouter,
            "opencode-go" => Self::OpencodeGo,
            "openai-compatible" => Self::OpenaiCompatible,
            "orcarouter" => Self::Orcarouter,
            "ollama" => Self::Ollama,
            "vllm" => Self::Vllm,
            "llamacpp" => Self::Llamacpp,
            "gemini" => Self::Gemini,
            "fake" => Self::Fake,
            _ => {
                return Err(Error::Config(format!(
                    "Unknown provider: {kind}; available: deepseek, openai, openrouter, \
                     opencode-go, openai-compatible, orcarouter, ollama, vllm, llamacpp, gemini, fake"
                )));
            }
        })
    }

    /// Registry key.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deepseek => "deepseek",
            Self::Openai => "openai",
            Self::Openrouter => "openrouter",
            Self::OpencodeGo => "opencode-go",
            Self::OpenaiCompatible => "openai-compatible",
            Self::Orcarouter => "orcarouter",
            Self::Ollama => "ollama",
            Self::Vllm => "vllm",
            Self::Llamacpp => "llamacpp",
            Self::Gemini => "gemini",
            Self::Fake => "fake",
        }
    }

    /// Default endpoint (oracle `default_base_url`).
    pub fn default_base_url(self) -> Option<String> {
        match self {
            Self::Deepseek => Some("https://api.deepseek.com".to_string()),
            Self::Openai => Some("https://api.openai.com/v1".to_string()),
            Self::Openrouter => Some("https://openrouter.ai/api/v1".to_string()),
            Self::OpencodeGo => Some("https://opencode.ai/zen/go/v1".to_string()),
            Self::OpenaiCompatible => None,
            Self::Orcarouter => Some("https://api.orcarouter.ai/v1".to_string()),
            Self::Ollama => Some("http://localhost:11434/v1".to_string()),
            Self::Vllm => Some("http://localhost:8000/v1".to_string()),
            Self::Llamacpp => Some("http://127.0.0.1:8093/v1".to_string()),
            Self::Gemini => Some("https://generativelanguage.googleapis.com".to_string()),
            Self::Fake => None,
        }
    }

    /// Default credential variable (oracle `default_api_key_env`).
    pub fn default_api_key_env(self) -> Option<&'static str> {
        match self {
            Self::Deepseek => Some("DEEPSEEK_API_KEY"),
            Self::Openai => Some("OPENAI_API_KEY"),
            Self::Openrouter => Some("OPENROUTER_API_KEY"),
            Self::OpencodeGo => Some("OPENCODE_API_KEY"),
            Self::OpenaiCompatible => None,
            Self::Orcarouter => Some("ORCAROUTER_API_KEY"),
            Self::Ollama | Self::Vllm | Self::Llamacpp => None,
            Self::Gemini => Some("GEMINI_API_KEY"),
            Self::Fake => None,
        }
    }

    /// Whether a credential is mandatory (oracle `requires_api_key`).
    pub fn requires_api_key(self) -> bool {
        matches!(
            self,
            Self::Deepseek
                | Self::Openai
                | Self::Openrouter
                | Self::OpencodeGo
                | Self::Orcarouter
                | Self::Gemini
        )
    }

    /// Whether an endpoint is mandatory (oracle `requires_base_url`).
    pub fn requires_base_url(self) -> bool {
        !matches!(self, Self::Fake)
    }

    /// Wire protocol version (oracle `protocol_version`; all 1 today).
    pub fn protocol_version(self) -> i64 {
        1
    }

    /// Validate connection extras against the adapter's connection-options
    /// schema (oracle `validate_connection`); returns canonical options.
    pub fn validate_connection_options(
        self,
        extra: &Map<String, Value>,
    ) -> Result<serde_json::Value> {
        match self {
            Self::OpenaiCompatible
            | Self::Orcarouter
            | Self::Ollama
            | Self::Vllm
            | Self::Llamacpp => {
                // Oracle `CompatibleConnectionOptions`: only `reasoning_style`.
                for key in extra.keys() {
                    if key != "reasoning_style" {
                        return Err(Error::Config(format!(
                            "llm.providers: unknown {} connection option {key:?}",
                            self.as_str()
                        )));
                    }
                }
                let style = extra
                    .get("reasoning_style")
                    .and_then(|v| v.as_str())
                    .unwrap_or("none");
                if !["none", "deepseek", "openai", "openrouter"].contains(&style) {
                    return Err(Error::Config(format!(
                        "llm.providers: invalid reasoning_style {style:?}"
                    )));
                }
                Ok(serde_json::json!({"reasoning_style": style}))
            }
            _ => {
                if let Some(key) = extra.keys().next() {
                    return Err(Error::Config(format!(
                        "llm.providers: unknown {} connection option {key:?}",
                        self.as_str()
                    )));
                }
                Ok(serde_json::json!({}))
            }
        }
    }

    /// Validate model options against the adapter schema (oracle
    /// `validate_model`); returns canonical options.
    pub fn validate_model_options(self, options: &Map<String, Value>) -> Result<serde_json::Value> {
        check_options(&Value::Object(options.clone()))?;
        match self {
            Self::Deepseek | Self::Openai | Self::Openrouter => {
                Ok(validate_thinking_options(options, self.default_thinking())?)
            }
            Self::OpencodeGo => validate_opencode_options(options),
            Self::OpenaiCompatible
            | Self::Orcarouter
            | Self::Ollama
            | Self::Vllm
            | Self::Llamacpp => validate_compatible_options(options),
            Self::Gemini => validate_gemini_options(options),
            Self::Fake => {
                if let Some(key) = options.keys().next() {
                    return Err(Error::Config(format!(
                        "llm.models: unknown fake option {key:?}"
                    )));
                }
                Ok(serde_json::json!({}))
            }
        }
    }

    /// Default `thinking` flag per kind.
    fn default_thinking(self) -> bool {
        matches!(self, Self::Deepseek | Self::Openai | Self::Openrouter)
    }

    /// Effective output limit (oracle `output_limit`): explicit wins, but
    /// thinking mode floors hints at 4096 and rejects smaller explicits.
    pub fn output_limit(
        self,
        options: &serde_json::Value,
        hint: Option<i64>,
        explicit: Option<i64>,
    ) -> Result<Option<i64>> {
        let thinking = options
            .get("thinking")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if let Some(explicit) = explicit {
            if thinking && explicit < 4096 {
                return Err(Error::Config(
                    "Thinking mode requires max_output_tokens >= 4096; disable thinking or increase the explicit limit"
                        .to_string(),
                ));
            }
            return Ok(Some(explicit));
        }
        Ok(match (thinking, hint) {
            (true, Some(hint)) => Some(hint.max(4096)),
            _ => hint,
        })
    }

    /// Validate a full connection envelope (oracle `validate_connection`).
    pub fn validate_connection(self, connection: &ProviderConfig) -> Result<()> {
        self.validate_connection_options(&connection.extra)?;
        if self.requires_base_url()
            && connection.base_url.is_none()
            && self.default_base_url().is_none()
        {
            return Err(Error::Config(format!(
                "Provider {} requires base_url",
                self.as_str()
            )));
        }
        Ok(())
    }

    /// Validate a full model profile (oracle `validate_model` via routing).
    pub fn validate_model_profile(self, model: &ModelConfig) -> Result<()> {
        self.validate_model_options(&model.options)?;
        self.output_limit(
            &self.validate_model_options(&model.options)?,
            None,
            model.max_output_tokens,
        )?;
        Ok(())
    }
}

/// Reject reserved keys anywhere in raw request overrides (oracle
/// `_check_options`; normalization strips `_`/`-` and lowercases).
fn check_options(value: &Value) -> Result<()> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let normalized: String = key
                    .chars()
                    .filter(|c| *c != '_' && *c != '-')
                    .flat_map(|c| c.to_lowercase())
                    .collect();
                if PROTECTED_OPTIONS.contains(&normalized.as_str()) {
                    return Err(Error::Config(format!("Reserved model option: {key}")));
                }
                check_options(child)?;
            }
        }
        Value::Array(items) => {
            for child in items {
                check_options(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Shared `{thinking, reasoning_effort, extra_body}` schema (DeepSeek,
/// OpenAI, OpenRouter).
fn validate_thinking_options(
    options: &Map<String, Value>,
    default_thinking: bool,
) -> Result<serde_json::Value> {
    for key in options.keys() {
        if !["thinking", "reasoning_effort", "extra_body"].contains(&key.as_str()) {
            return Err(Error::Config(format!("llm.models: unknown option {key:?}")));
        }
    }
    let thinking = options
        .get("thinking")
        .map(|v| {
            v.as_bool().ok_or_else(|| {
                Error::Config("llm.models.options.thinking: must be a boolean".to_string())
            })
        })
        .transpose()?
        .unwrap_or(default_thinking);
    let reasoning_effort = options
        .get("reasoning_effort")
        .map(|v| {
            v.as_str().map(str::to_string).ok_or_else(|| {
                Error::Config("llm.models.options.reasoning_effort: must be a string".to_string())
            })
        })
        .transpose()?
        .unwrap_or_else(|| "high".to_string());
    let extra_body = options
        .get("extra_body")
        .map(|v| {
            v.as_object().cloned().ok_or_else(|| {
                Error::Config("llm.models.options.extra_body: must be a mapping".to_string())
            })
        })
        .transpose()?
        .unwrap_or_default();
    Ok(serde_json::json!({
        "thinking": thinking,
        "reasoning_effort": reasoning_effort,
        "extra_body": extra_body,
    }))
}

/// OpenCode Go options: `reasoning_effort` restricted to low/medium/high.
fn validate_opencode_options(options: &Map<String, Value>) -> Result<serde_json::Value> {
    for key in options.keys() {
        if !["thinking", "reasoning_effort", "extra_body"].contains(&key.as_str()) {
            return Err(Error::Config(format!("llm.models: unknown option {key:?}")));
        }
    }
    let thinking = options
        .get("thinking")
        .map(|v| {
            v.as_bool().ok_or_else(|| {
                Error::Config("llm.models.options.thinking: must be a boolean".to_string())
            })
        })
        .transpose()?
        .unwrap_or(false);
    let reasoning_effort = options
        .get("reasoning_effort")
        .map(|v| v.as_str().map(str::to_string))
        .unwrap_or(Some("low".to_string()));
    let reasoning_effort = reasoning_effort.ok_or_else(|| {
        Error::Config("llm.models.options.reasoning_effort: must be a string".to_string())
    })?;
    if !["low", "medium", "high"].contains(&reasoning_effort.as_str()) {
        return Err(Error::Config(format!(
            "llm.models.options.reasoning_effort: must be low, medium or high, got {reasoning_effort:?}"
        )));
    }
    let extra_body = options
        .get("extra_body")
        .map(|v| {
            v.as_object().cloned().ok_or_else(|| {
                Error::Config("llm.models.options.extra_body: must be a mapping".to_string())
            })
        })
        .transpose()?
        .unwrap_or_default();
    Ok(serde_json::json!({
        "thinking": thinking,
        "reasoning_effort": reasoning_effort,
        "extra_body": extra_body,
    }))
}

/// Generic compatible options: `request_overrides` passthrough +
/// `json_response_fallback` gate.
fn validate_compatible_options(options: &Map<String, Value>) -> Result<serde_json::Value> {
    for key in options.keys() {
        if ![
            "thinking",
            "reasoning_effort",
            "json_response_fallback",
            "request_overrides",
        ]
        .contains(&key.as_str())
        {
            return Err(Error::Config(format!("llm.models: unknown option {key:?}")));
        }
    }
    let thinking = options
        .get("thinking")
        .map(|v| {
            v.as_bool().ok_or_else(|| {
                Error::Config("llm.models.options.thinking: must be a boolean".to_string())
            })
        })
        .transpose()?
        .unwrap_or(false);
    let reasoning_effort = options
        .get("reasoning_effort")
        .map(|v| {
            v.as_str().map(str::to_string).ok_or_else(|| {
                Error::Config("llm.models.options.reasoning_effort: must be a string".to_string())
            })
        })
        .transpose()?
        .unwrap_or_else(|| "high".to_string());
    let fallback = options
        .get("json_response_fallback")
        .map(|v| v.as_str().map(str::to_string))
        .unwrap_or(Some("none".to_string()));
    let fallback = fallback.ok_or_else(|| {
        Error::Config("llm.models.options.json_response_fallback: must be a string".to_string())
    })?;
    if !["none", "reasoning_content"].contains(&fallback.as_str()) {
        return Err(Error::Config(format!(
            "llm.models.options.json_response_fallback: must be none or reasoning_content, got {fallback:?}"
        )));
    }
    let overrides = options
        .get("request_overrides")
        .map(|v| {
            v.as_object().cloned().ok_or_else(|| {
                Error::Config("llm.models.options.request_overrides: must be a mapping".to_string())
            })
        })
        .transpose()?
        .unwrap_or_default();
    Ok(serde_json::json!({
        "thinking": thinking,
        "reasoning_effort": reasoning_effort,
        "json_response_fallback": fallback,
        "request_overrides": overrides,
    }))
}

/// Gemini options: mutually exclusive thinking level/budget + temperature.
fn validate_gemini_options(options: &Map<String, Value>) -> Result<serde_json::Value> {
    for key in options.keys() {
        if ![
            "thinking_level",
            "thinking_budget",
            "temperature",
            "extra_body",
        ]
        .contains(&key.as_str())
        {
            return Err(Error::Config(format!("llm.models: unknown option {key:?}")));
        }
    }
    let level = options.get("thinking_level").and_then(|v| v.as_str());
    let budget = options.get("thinking_budget").and_then(|v| v.as_i64());
    if level.is_some() && budget.is_some() {
        return Err(Error::Config(
            "llm.models.options: thinking_level and thinking_budget are mutually exclusive"
                .to_string(),
        ));
    }
    if let Some(v) = options.get("thinking_budget") {
        if !v.is_null() && v.as_i64().is_none() {
            return Err(Error::Config(
                "llm.models.options.thinking_budget: must be an integer".to_string(),
            ));
        }
    }
    if let Some(v) = options.get("temperature") {
        if !v.is_null() && v.as_f64().is_none() {
            return Err(Error::Config(
                "llm.models.options.temperature: must be a number".to_string(),
            ));
        }
    }
    let extra_body = options
        .get("extra_body")
        .map(|v| {
            v.as_object().cloned().ok_or_else(|| {
                Error::Config("llm.models.options.extra_body: must be a mapping".to_string())
            })
        })
        .transpose()?
        .unwrap_or_default();
    Ok(serde_json::json!({
        "thinking_level": level,
        "thinking_budget": budget,
        "temperature": options.get("temperature"),
        "extra_body": extra_body,
    }))
}

/// Built-in tier presets per kind (oracle `preset_models`).
pub fn preset_models(kind: ProviderKind) -> Result<BTreeMap<String, (String, Value)>> {
    let presets = |model: &str, options: Value| {
        ["strong", "cheap", "fast"]
            .into_iter()
            .map(|tier| (tier.to_string(), (model.to_string(), options.clone())))
            .collect()
    };
    match kind {
        ProviderKind::Deepseek => {
            let options = validate_thinking_options(&Map::new(), true)?;
            Ok(presets("deepseek-flash", options))
        }
        ProviderKind::Fake => Ok(presets("fake", serde_json::json!({}))),
        ProviderKind::Llamacpp => {
            let options = validate_compatible_options(&Map::new())?;
            Ok([
                (
                    "strong".to_string(),
                    ("lokal27b".to_string(), options.clone()),
                ),
                (
                    "cheap".to_string(),
                    ("lokal9b".to_string(), options.clone()),
                ),
                ("fast".to_string(), ("lokal9b".to_string(), options)),
            ]
            .into_iter()
            .collect())
        }
        ProviderKind::Gemini => {
            let options = validate_gemini_options(&Map::new())?;
            Ok(presets("gemini-3.6-flash", options))
        }
        _ => Err(Error::Config(format!(
            "Provider {:?} has no preset; configure explicit model profiles",
            kind.as_str()
        ))),
    }
}
