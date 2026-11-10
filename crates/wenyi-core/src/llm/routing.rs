//! Pure route resolution into immutable inference identities.
//! Oracle: `llm/routing.py`.
//!
//! Compiles every registered operation to a `ResolvedRoute` without
//! credentials, client construction, or network I/O.

use std::collections::BTreeMap;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config::LlmConfig;
use crate::llm::operations::{require_operation, OperationSpec};
use crate::llm::providers::ProviderKind;
use crate::{Error, Result};

/// Canonical JSON: sorted keys, compact separators, unescaped Unicode
/// (oracle `canonical_json`).
pub fn canonical_json(value: &serde_json::Value) -> String {
    // serde_json Map is BTreeMap-backed only with `preserve_order` off;
    // re-sort explicitly to guarantee oracle-identical bytes.
    fn sort(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut sorted = serde_json::Map::new();
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for key in keys {
                    sorted.insert(key.clone(), sort(&map[key]));
                }
                serde_json::Value::Object(sorted)
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(sort).collect())
            }
            other => other.clone(),
        }
    }
    let sorted = sort(value);
    serde_json::to_string(&sorted).expect("canonical JSON")
}

/// SHA-256 hex of canonical JSON (oracle `identity`).
pub fn identity(value: &serde_json::Value) -> String {
    let digest = Sha256::digest(canonical_json(value).as_bytes());
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

/// One compiled operation route.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedRoute {
    /// Operation ID.
    pub operation: String,
    /// Where the route came from (`llm.routes.*`, `inherits *`, `default tier *`).
    pub origin: String,
    /// Product tier (None for direct model selection).
    pub tier: Option<String>,
    /// Model profile name.
    pub profile: String,
    /// Connection name.
    pub provider: String,
    /// Provider kind.
    pub provider_kind: String,
    /// Effective endpoint.
    pub endpoint: Option<String>,
    /// Model identifier.
    pub model: String,
    /// Canonical validated options JSON.
    pub options_json: String,
    /// Canonical validated connection-options JSON.
    pub connection_options_json: String,
    /// Effective output cap.
    pub max_output_tokens: Option<i64>,
    /// Profile input cap.
    pub max_input_tokens: Option<i64>,
    /// Identity of the physical provider (kind + endpoint).
    pub provider_identity: String,
    /// Identity of the physical model (provider + model + options + caps).
    pub model_identity: String,
    /// Full inference fingerprint (operation + protocol + model).
    pub fingerprint: String,
    /// Explicit fallback profiles.
    pub fallbacks: Vec<String>,
}

/// Resolve one operation+profile to a route (oracle `model_route`).
#[allow(clippy::too_many_arguments)]
pub fn model_route(
    config: &LlmConfig,
    operation: &str,
    profile: &str,
    origin: String,
    tier: Option<String>,
    fallbacks: Vec<String>,
    output_hint: Option<i64>,
) -> Result<ResolvedRoute> {
    let spec = require_operation(operation)?;
    let model = config
        .models
        .get(profile)
        .ok_or_else(|| Error::Config(format!("llm.models.{profile}: unknown model profile")))?;
    let connection = config.providers.get(&model.provider).ok_or_else(|| {
        Error::Config(format!(
            "llm.models.{profile}.provider: unknown connection {:?}",
            model.provider
        ))
    })?;
    let kind = ProviderKind::require(&connection.kind)?;
    let options = kind.validate_model_options(&model.options)?;
    if let Some(hint) = output_hint {
        if hint <= 0 {
            return Err(Error::Config("max_tokens must be positive".to_string()));
        }
    }
    let limit = kind.output_limit(
        &options,
        output_hint.or(spec.output_tokens),
        model.max_output_tokens,
    )?;
    let endpoint = connection
        .base_url
        .clone()
        .or_else(|| kind.default_base_url());
    let connection_options = kind.validate_connection_options(&connection.extra)?;
    let physical_provider = serde_json::json!({
        "kind": connection.kind,
        "endpoint": endpoint.as_deref().map(|e| e.trim_end_matches('/')),
    });
    let physical_model = serde_json::json!({
        "provider": physical_provider,
        "model": model.model,
        "adapter_protocol": kind.protocol_version(),
        "options": options,
        "max_output_tokens": limit,
        "max_input_tokens": model.max_input_tokens,
        "connection_options": connection_options,
    });
    Ok(ResolvedRoute {
        operation: operation.to_string(),
        origin,
        tier,
        profile: profile.to_string(),
        provider: model.provider.clone(),
        provider_kind: connection.kind.clone(),
        endpoint,
        model: model.model.clone(),
        options_json: canonical_json(&options),
        connection_options_json: canonical_json(&connection_options),
        max_output_tokens: limit,
        max_input_tokens: model.max_input_tokens,
        provider_identity: identity(&physical_provider),
        model_identity: identity(&physical_model),
        fingerprint: identity(&serde_json::json!({
            "operation": operation,
            "protocol": spec.protocol_version,
            "inference": physical_model,
        })),
        fallbacks,
    })
}

/// Compile all registered routes (oracle `resolve_routes`).
pub fn resolve_routes(config: &LlmConfig) -> Result<BTreeMap<String, ResolvedRoute>> {
    let mut resolved: BTreeMap<String, ResolvedRoute> = BTreeMap::new();
    for operation in crate::llm::operations::operation_ids() {
        resolve_one(config, operation, &mut resolved)?;
    }
    Ok(resolved)
}

fn resolve_one(
    config: &LlmConfig,
    operation: &str,
    resolved: &mut BTreeMap<String, ResolvedRoute>,
) -> Result<ResolvedRoute> {
    let spec = require_operation(operation)?;
    resolve_one_spec(config, operation, spec, resolved)
}

fn resolve_one_spec(
    config: &LlmConfig,
    operation: &str,
    spec: &OperationSpec,
    resolved: &mut BTreeMap<String, ResolvedRoute>,
) -> Result<ResolvedRoute> {
    let (tier, profile, fallbacks, origin) = match config.routes.get(operation) {
        Some(route) => {
            let profile = match (&route.model, &route.tier) {
                (Some(model), None) => model.clone(),
                (None, Some(tier)) => config.tiers.get(tier).cloned().ok_or_else(|| {
                    Error::Config(format!(
                        "llm.routes.{operation}.tier: unknown tier {tier:?}"
                    ))
                })?,
                _ => {
                    return Err(Error::Config(format!(
                        "llm.routes.{operation}: requires exactly one of model or tier"
                    )));
                }
            };
            (
                route.tier.clone(),
                profile,
                route.fallbacks.clone(),
                format!("llm.routes.{operation}"),
            )
        }
        None => match spec.inherits {
            Some(parent_id) => {
                let parent = resolve_one(config, parent_id, resolved)?;
                (
                    parent.tier,
                    parent.profile,
                    parent.fallbacks,
                    format!("inherits {parent_id}"),
                )
            }
            None => {
                let tier = spec.tier.ok_or_else(|| {
                    Error::Config(format!(
                        "operation {operation}: no default tier and no inherits parent"
                    ))
                })?;
                let profile = config.tiers.get(tier).cloned().ok_or_else(|| {
                    Error::Config(format!("llm.tiers.{tier}: unknown model profile"))
                })?;
                (
                    Some(tier.to_string()),
                    profile,
                    Vec::new(),
                    format!("default tier {tier}"),
                )
            }
        },
    };
    // Fix the parent origin string (resolve_one returns the route, not the id).
    let origin = match spec.inherits {
        Some(parent) if !config.routes.contains_key(operation) => format!("inherits {parent}"),
        _ => origin,
    };
    if !fallbacks.is_empty() && spec.resumable_conversation {
        return Err(Error::Config(format!(
            "{operation}: model failover is not allowed inside resumable evidence conversations"
        )));
    }
    let route = model_route(config, operation, &profile, origin, tier, fallbacks, None)?;
    resolved.insert(operation.to_string(), route.clone());
    Ok(route)
}

/// Fingerprint snapshot for a set of operations (oracle `inference_snapshot`).
pub fn inference_snapshot(
    config: &LlmConfig,
    operations: &[String],
) -> Result<BTreeMap<String, serde_json::Value>> {
    let routes = resolve_routes(config)?;
    let mut snapshot = BTreeMap::new();
    let mut sorted: Vec<&String> = operations.iter().collect();
    sorted.sort();
    for operation in sorted {
        let route = routes
            .get(operation.as_str())
            .ok_or_else(|| Error::Config(format!("Unknown model operation: {operation}")))?;
        let fallbacks = route
            .fallbacks
            .iter()
            .map(|profile| {
                model_route(
                    config,
                    operation,
                    profile,
                    "fallback".to_string(),
                    None,
                    Vec::new(),
                    None,
                )
                .map(|r| r.fingerprint)
            })
            .collect::<Result<Vec<_>>>()?;
        snapshot.insert(
            operation.clone(),
            serde_json::json!({"primary": route.fingerprint, "fallbacks": fallbacks}),
        );
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_without_tier_or_inherits_is_config_error() {
        // The registry guarantees every operation carries exactly one of
        // tier/inherits, so synthesize the invalid spec directly: this pins
        // the typed-error path for the (impossible) corrupt-registry case.
        let spec = OperationSpec {
            id: "synthetic.broken",
            description: "synthetic",
            tier: None,
            inherits: None,
            output_tokens: None,
            workflows: &[],
            flags: &[],
            review: false,
            resumable_conversation: false,
            protocol_version: 1,
        };
        let mut resolved = BTreeMap::new();
        let err = resolve_one_spec(
            &LlmConfig::default(),
            "synthetic.broken",
            &spec,
            &mut resolved,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("no default tier and no inherits parent"),
            "unexpected error: {err}"
        );
    }
}
