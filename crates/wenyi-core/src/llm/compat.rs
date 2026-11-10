//! OpenAI-compatible wire protocol. Oracle: `llm/providers/_openai_compatible.py`.
//!
//! Shared by all nine compatible kinds: request bodies, response
//! extraction, usage normalization. Raw `reqwest` POSTs — no provider SDKs.

use serde_json::{Map, Value};

use crate::llm::errors::{ProviderError, ProviderErrorKind};
use crate::llm::usage::{make_usage_sample, read_usage_int, UsageSample};

/// JSON-mode instruction appended to system + user messages.
pub const JSON_MODE_INSTRUCTION: &str = "Output must be valid json.";

/// One chat message.
#[derive(Debug, Clone)]
pub struct Message {
    /// `system`, `user`, `assistant`.
    pub role: String,
    /// Message text.
    pub content: String,
}

/// Build the base Chat Completions body (oracle `base_request_kwargs`).
pub fn base_request_body(model: &str, messages: &[Message], json_mode: bool) -> serde_json::Value {
    let mut request_messages: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
        .collect();
    if json_mode {
        let mut system_index = None;
        for (index, message) in request_messages.iter().enumerate() {
            if message.get("role").and_then(|r| r.as_str()) == Some("system") {
                system_index = Some(index);
                break;
            }
        }
        match system_index {
            Some(index) => {
                let content = request_messages[index]
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                request_messages[index]["content"] =
                    Value::String(format!("{content}\n\n{JSON_MODE_INSTRUCTION}"));
            }
            None => request_messages.insert(
                0,
                serde_json::json!({"role": "system", "content": JSON_MODE_INSTRUCTION}),
            ),
        }
        // Gateways validating only user content need the instruction there too.
        for message in request_messages.iter_mut().rev() {
            if message.get("role").and_then(|r| r.as_str()) == Some("user") {
                let content = message
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                if !content.to_lowercase().contains("json") {
                    message["content"] =
                        Value::String(format!("{content}\n\n{JSON_MODE_INSTRUCTION}"));
                }
                break;
            }
        }
    }
    let mut body = serde_json::json!({
        "model": model,
        "messages": request_messages,
        "stream": false,
    });
    if json_mode {
        body["response_format"] = serde_json::json!({"type": "json_object"});
    }
    body
}

/// Recursively merge request bodies, preferring user values (oracle
/// `deep_merge`).
pub fn deep_merge(base: &Map<String, Value>, override_: &Map<String, Value>) -> Map<String, Value> {
    let mut merged = base.clone();
    for (key, value) in override_ {
        match (
            merged.get(key).and_then(|v| v.as_object()),
            value.as_object(),
        ) {
            (Some(current), Some(next)) => {
                merged[key] = Value::Object(deep_merge(current, next));
            }
            _ => {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    merged
}

/// Normalize OpenAI-style nested cache details (oracle
/// `normalize_openai_usage`).
pub fn normalize_openai_usage(usage: Option<&Map<String, Value>>) -> Option<UsageSample> {
    let usage = usage?;
    let details = usage
        .get("prompt_tokens_details")
        .and_then(|v| v.as_object());
    let (hit, miss) = match details.and_then(|d| d.get("cached_tokens")) {
        None => (0, 0),
        Some(_) => {
            let hit = details
                .map(|d| read_usage_int(d.get("cached_tokens")))
                .unwrap_or(0);
            let miss = (read_usage_int(usage.get("prompt_tokens")) - hit).max(0);
            (hit, miss)
        }
    };
    Some(make_usage_sample(usage, hit, miss))
}

/// Normalize DeepSeek's top-level cache counters.
pub fn normalize_deepseek_usage(usage: Option<&Map<String, Value>>) -> Option<UsageSample> {
    let usage = usage?;
    Some(make_usage_sample(
        usage,
        read_usage_int(usage.get("prompt_cache_hit_tokens")),
        read_usage_int(usage.get("prompt_cache_miss_tokens")),
    ))
}

/// Extracted chat completion: text + optional usage.
pub struct Completion {
    /// Model text.
    pub text: String,
    /// Raw `usage` object for adapter-specific normalization.
    pub usage: Option<Map<String, Value>>,
}

/// Extract content from a Chat Completions response (oracle `_request`
/// response handling: truncation → `TruncatedResponseError`, blank →
/// `EmptyResponseError` with refusal/tool-call/reasoning diagnostics).
pub fn extract_completion(
    kind: &str,
    response: &Value,
    json_mode: bool,
    json_fallback: Option<&str>,
) -> Result<Completion, ProviderError> {
    let choice = response
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first());
    let (message, finish_reason) = match choice {
        Some(choice) => (
            choice.get("message"),
            choice
                .get("finish_reason")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_lowercase(),
        ),
        None => (None, String::new()),
    };
    if finish_reason == "length" {
        return Err(ProviderError::transport(
            ProviderErrorKind::TruncatedResponse,
            "OpenAI-compatible response was truncated at the token limit",
        ));
    }
    let content = message
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    if !content.trim().is_empty() {
        return Ok(Completion {
            text: content.to_string(),
            usage: response.get("usage").and_then(|u| u.as_object()).cloned(),
        });
    }
    // Blank content: optional JSON fallback, else a diagnostic error.
    if json_mode {
        if let Some(fallback) = json_fallback {
            if fallback.trim().is_empty() || serde_json::from_str::<Value>(fallback).is_err() {
                return Err(ProviderError::transport(
                    ProviderErrorKind::EmptyResponse,
                    format!("{kind} configured JSON fallback response is invalid JSON"),
                ));
            }
            return Ok(Completion {
                text: fallback.to_string(),
                usage: response.get("usage").and_then(|u| u.as_object()).cloned(),
            });
        }
    }
    let refusal = message
        .and_then(|m| m.get("refusal"))
        .is_some_and(|r| !r.is_null());
    let tool_calls = message
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array())
        .map(|t| t.len())
        .unwrap_or(0);
    let reasoning = message
        .and_then(|m| m.get("reasoning_content"))
        .and_then(|r| r.as_str())
        .is_some();
    Err(ProviderError::transport(
        ProviderErrorKind::EmptyResponse,
        format!(
            "{kind} response content is empty (finish_reason={}, refusal={refusal}, \
             tool_calls={tool_calls}, reasoning_content={})",
            if finish_reason.is_empty() {
                "none"
            } else {
                &finish_reason
            },
            if reasoning { "present" } else { "absent" },
        ),
    ))
}

/// Per-kind request-body builder (oracle `build_request_kwargs` per module).
pub fn build_request_body(
    kind: crate::llm::providers::ProviderKind,
    model: &str,
    options: &Value,
    connection_options: &Value,
    messages: &[Message],
    json_mode: bool,
    max_tokens: Option<i64>,
) -> serde_json::Value {
    use crate::llm::providers::ProviderKind;
    let mut body = base_request_body(model, messages, json_mode);
    let thinking = options
        .get("thinking")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let effort = options
        .get("reasoning_effort")
        .and_then(|v| v.as_str())
        .unwrap_or("high");
    let user_extra = options
        .get("extra_body")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    match kind {
        ProviderKind::Openai => {
            body["reasoning_effort"] = Value::String(if thinking {
                effort.to_string()
            } else {
                "none".to_string()
            });
            if !user_extra.is_empty() {
                body["extra_body"] = Value::Object(deep_merge(&Map::new(), &user_extra));
            }
            if let Some(max_tokens) = max_tokens {
                body["max_completion_tokens"] = Value::from(max_tokens);
            }
        }
        ProviderKind::Deepseek => {
            let mut extra = Map::new();
            extra.insert(
                "thinking".to_string(),
                serde_json::json!({"type": if thinking { "enabled" } else { "disabled" }}),
            );
            if thinking {
                body["reasoning_effort"] = Value::String(effort.to_string());
            }
            body["extra_body"] = Value::Object(deep_merge(&extra, &user_extra));
            if let Some(max_tokens) = max_tokens {
                body["max_tokens"] = Value::from(max_tokens);
            }
        }
        ProviderKind::Openrouter => {
            let mut extra = Map::new();
            extra.insert(
                "reasoning".to_string(),
                if thinking {
                    serde_json::json!({"effort": effort})
                } else {
                    serde_json::json!({"enabled": false})
                },
            );
            body["extra_body"] = Value::Object(deep_merge(&extra, &user_extra));
            if let Some(max_tokens) = max_tokens {
                body["max_tokens"] = Value::from(max_tokens);
            }
        }
        ProviderKind::OpencodeGo => {
            let mut extra = user_extra;
            extra
                .entry("reasoning_effort".to_string())
                .or_insert_with(|| Value::String(effort.to_string()));
            if !extra.is_empty() {
                body["extra_body"] = Value::Object(deep_merge(&Map::new(), &extra));
            }
            if let Some(max_tokens) = max_tokens {
                body["max_tokens"] = Value::from(max_tokens);
            }
        }
        ProviderKind::OpenaiCompatible
        | ProviderKind::Orcarouter
        | ProviderKind::Ollama
        | ProviderKind::Vllm
        | ProviderKind::Llamacpp => {
            let style = connection_options
                .get("reasoning_style")
                .and_then(|v| v.as_str())
                .unwrap_or("none");
            let mut extra = Map::new();
            match style {
                "deepseek" => {
                    extra.insert(
                        "thinking".to_string(),
                        serde_json::json!({"type": if thinking { "enabled" } else { "disabled" }}),
                    );
                    if thinking {
                        body["reasoning_effort"] = Value::String(effort.to_string());
                    }
                }
                "openai" => {
                    body["reasoning_effort"] = Value::String(if thinking {
                        effort.to_string()
                    } else {
                        "none".to_string()
                    });
                }
                "openrouter" => {
                    extra.insert(
                        "reasoning".to_string(),
                        if thinking {
                            serde_json::json!({"effort": effort})
                        } else {
                            serde_json::json!({"enabled": false})
                        },
                    );
                }
                _ => {}
            }
            let overrides = options
                .get("request_overrides")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            let merged = deep_merge(&extra, &overrides);
            if !merged.is_empty() {
                body["extra_body"] = Value::Object(merged);
            }
            if let Some(max_tokens) = max_tokens {
                body["max_tokens"] = Value::from(max_tokens);
            }
        }
        ProviderKind::Gemini | ProviderKind::Fake => {
            unreachable!("non-compatible kinds never reach the shared wire core")
        }
    }
    body
}
