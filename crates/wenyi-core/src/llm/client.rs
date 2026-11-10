//! Routed workflow client. Oracle: `llm/router.py` + `llm/base.py`.
//!
//! Freezes one routing plan per invocation: token preflight, permit
//! admission, shared-retry generation, explicit stateless failover, and
//! redacted lifecycle events. Adapters never own cumulative usage.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::config::LlmConfig;
use crate::document::tokens::count_tokens;
use crate::llm::compat::Message;
use crate::llm::errors::{ProviderError, ProviderFailure};
use crate::llm::fake::FakeProvider;
use crate::llm::limits::RequestLimits;
use crate::llm::operations::require_operation;
use crate::llm::providers::ProviderKind;
use crate::llm::routing::{model_route, resolve_routes, ResolvedRoute};
use crate::llm::transport::{
    backoff, classify_reqwest, generate_with_retry, http_client, is_loopback, safe_error, Emit,
    HttpConfig, RequestContext, ResolvedModel,
};
use crate::llm::usage::{UsageAttribution, UsageSample, UsageTracker};
use crate::redact::{validate_credential, Redactor};
use crate::{Error, Result};

/// One live provider connection: HTTP client + credentials + retry config.
struct Connection {
    kind: ProviderKind,
    config: HttpConfig,
    endpoint: Option<String>,
    credential: Option<String>,
    client: reqwest::Client,
}

/// Routed client: frozen routes, shared limits, one usage ledger.
pub struct RoutedClient {
    config: LlmConfig,
    credentials: Option<BTreeMap<String, Option<String>>>,
    redact: Option<Redactor>,
    routes: BTreeMap<String, ResolvedRoute>,
    limits: RequestLimits,
    connections: Mutex<BTreeMap<String, Connection>>,
    usage: Arc<UsageTracker>,
    emit: Emit,
}

impl RoutedClient {
    /// Freeze routes + limits for one invocation.
    pub fn new(
        config: LlmConfig,
        credentials: Option<BTreeMap<String, Option<String>>>,
        emit: Emit,
    ) -> Result<Self> {
        let redact = match &credentials {
            Some(snapshot) => {
                for secret in snapshot.values() {
                    validate_credential(secret.as_deref())?;
                }
                Some(Redactor::new(snapshot.values().filter_map(|s| s.clone())))
            }
            None => None,
        };
        let routes = resolve_routes(&config)?;
        // Validate every reachable connection + profile eagerly (oracle
        // validates the graph at config load; adapters validate at use).
        for route in routes.values() {
            let connection = config
                .providers
                .get(&route.provider)
                .ok_or_else(|| Error::Config(format!("Unknown connection {:?}", route.provider)))?;
            let kind = ProviderKind::require(&connection.kind)?;
            kind.validate_connection(connection)?;
            let model = config.models.get(&route.profile).ok_or_else(|| {
                Error::Config(format!("Unknown model profile {:?}", route.profile))
            })?;
            kind.validate_model_profile(model)?;
        }
        Ok(Self {
            limits: RequestLimits::new(config.clone()),
            config,
            credentials,
            redact,
            routes,
            connections: Mutex::new(BTreeMap::new()),
            usage: Arc::new(UsageTracker::new()),
            emit,
        })
    }

    /// Cancel cooperatively.
    pub fn cancel(&self) {
        self.limits.cancel();
    }

    /// Current usage summary.
    pub fn summary(&self) -> serde_json::Map<String, serde_json::Value> {
        self.usage.summary()
    }

    /// Validate credentials for operations (all when `None`).
    pub fn validate_credentials(&self, operations: Option<&[String]>) -> Result<()> {
        let mut connections: Vec<String> = Vec::new();
        let ops: Vec<String> = match operations {
            Some(ops) => ops.to_vec(),
            None => self.routes.keys().cloned().collect(),
        };
        for operation in &ops {
            require_operation(operation)?;
            let route = &self.routes[operation];
            connections.push(route.provider.clone());
            for profile in &route.fallbacks {
                let model =
                    self.config.models.get(profile).ok_or_else(|| {
                        Error::Config(format!("Unknown model profile {profile:?}"))
                    })?;
                connections.push(model.provider.clone());
            }
        }
        connections.sort();
        connections.dedup();
        for connection in connections {
            self.credential_for(&connection, true)?;
        }
        Ok(())
    }

    /// List models a connection offers (oracle `list_models` per adapter).
    /// Compat kinds read `GET /models` (following `next_page` cursors like
    /// the SDK); Gemini reads `GET /v1beta/models` filtered to
    /// text-generation actions; `fake` returns `["fake"]`.
    pub async fn list_models(&self, connection: &str) -> Result<Vec<String>> {
        let cfg = self
            .config
            .providers
            .get(connection)
            .ok_or_else(|| Error::Config(format!("Unknown connection {connection:?}")))?;
        let kind = ProviderKind::require(&cfg.kind)?;
        if kind == ProviderKind::Fake {
            return Ok(vec!["fake".to_string()]);
        }
        let endpoint = cfg
            .base_url
            .clone()
            .or_else(|| kind.default_base_url())
            .ok_or_else(|| {
                Error::Config(format!("Provider {} requires base_url", kind.as_str()))
            })?;
        let http_config = HttpConfig::from_connection(cfg, Some(endpoint.clone()));
        let fresh = http_client(&http_config)
            .map_err(|_| Error::Provider(format!("{}: request failed", kind.as_str())))?;
        let _ = &fresh;
        let credential = self.credential_for(connection, false)?;
        // Reuse the pooled connection client when present.
        let pooled = self
            .connections
            .lock()
            .expect("connections lock")
            .get(connection)
            .map(|c| c.client.clone());
        let client = pooled.unwrap_or(fresh);
        if kind == ProviderKind::Gemini {
            return Self::list_gemini_models(&client, &endpoint, credential.as_deref())
                .await
                .map_err(|e| self.provider_error(kind, e));
        }
        Self::list_compat_models(&client, &endpoint, credential.as_deref(), kind)
            .await
            .map_err(|e| self.provider_error(kind, e))
    }

    fn provider_error(&self, kind: ProviderKind, error: ProviderError) -> Error {
        let failure = ProviderFailure::describe(&error);
        if self.credentials.is_none() {
            return Error::Provider(format!(
                "{}: {} ({})",
                kind.as_str(),
                failure.message,
                failure.category
            ));
        }
        safe_error(&error, kind.as_str(), "models.list")
    }

    async fn list_compat_models(
        client: &reqwest::Client,
        endpoint: &str,
        credential: Option<&str>,
        kind: ProviderKind,
    ) -> Result<Vec<String>, ProviderError> {
        use crate::llm::errors::ProviderErrorKind;
        let mut names = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..100 {
            let mut url = format!("{}/models", endpoint.trim_end_matches('/'));
            if let Some(cursor) = &cursor {
                url.push_str(&format!("?after={cursor}"));
            }
            let mut request = client.get(&url);
            if let Some(key) = credential {
                request = request.bearer_auth(key);
            }
            if kind == ProviderKind::OpencodeGo {
                request = request
                    .header("User-Agent", "wenyi")
                    .header("x-opencode-session", uuid_hex());
            }
            let response = request.send().await.map_err(classify_reqwest)?;
            let status = response.status().as_u16();
            if !(200..300).contains(&status) {
                let headers: Vec<(String, String)> = response
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                    .collect();
                let _ = response.bytes().await;
                return Err(ProviderError::http(status, &headers, None));
            }
            let payload: serde_json::Value = response.json().await.map_err(|_| {
                ProviderError::transport(ProviderErrorKind::Unknown, "invalid provider response")
            })?;
            let items = payload.get("data").and_then(|d| d.as_array());
            match items {
                Some(items) => {
                    for item in items {
                        if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                            if !id.is_empty() {
                                names.push(id.to_string());
                            }
                        }
                    }
                    cursor = payload
                        .get("next_page")
                        .or_else(|| payload.get("next_cursor"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    if cursor.is_none() {
                        break;
                    }
                }
                None => break,
            }
        }
        Ok(names)
    }

    async fn list_gemini_models(
        client: &reqwest::Client,
        endpoint: &str,
        credential: Option<&str>,
    ) -> Result<Vec<String>, ProviderError> {
        use crate::llm::errors::ProviderErrorKind;
        let url = format!("{}/v1beta/models", endpoint.trim_end_matches('/'));
        let mut request = client.get(&url);
        if let Some(key) = credential {
            request = request.header("x-goog-api-key", key);
        }
        let response = request.send().await.map_err(classify_reqwest)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            let _ = response.bytes().await;
            return Err(ProviderError::http(status, &headers, None));
        }
        let payload: serde_json::Value = response.json().await.map_err(|_| {
            ProviderError::transport(ProviderErrorKind::Unknown, "invalid provider response")
        })?;
        let mut names = Vec::new();
        if let Some(models) = payload.get("models").and_then(|m| m.as_array()) {
            for item in models {
                let actions = item
                    .get("supportedActions")
                    .and_then(|a| a.as_array())
                    .map(|actions| {
                        actions
                            .iter()
                            .filter_map(|a| a.as_str())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if !name.is_empty() && (actions.is_empty() || actions.contains(&"generateContent"))
                {
                    names.push(name.strip_prefix("models/").unwrap_or(name).to_string());
                }
            }
        }
        Ok(names)
    }

    /// Resolve a credential: injected snapshot or environment.
    fn credential_for(&self, connection: &str, validate: bool) -> Result<Option<String>> {
        let cfg = self
            .config
            .providers
            .get(connection)
            .ok_or_else(|| Error::Config(format!("Unknown connection {connection:?}")))?;
        let kind = ProviderKind::require(&cfg.kind)?;
        let env = cfg
            .api_key_env
            .clone()
            .or_else(|| kind.default_api_key_env().map(str::to_string));
        let credential = match &self.credentials {
            Some(snapshot) => snapshot.get(connection).cloned().flatten(),
            None => env.as_deref().and_then(|var| std::env::var(var).ok()),
        };
        if validate {
            match &self.credentials {
                Some(_) => {
                    if credential.as_deref().is_none_or(|c| c.is_empty())
                        && (kind.requires_api_key() || env.is_some())
                    {
                        return Err(Error::Config(format!(
                            "Provider {} has no configured credential",
                            kind.as_str()
                        )));
                    }
                }
                None => {
                    if let Some(var) = &env {
                        if std::env::var(var)
                            .map(|v| v.trim().is_empty())
                            .unwrap_or(true)
                        {
                            return Err(Error::Config(format!(
                                "Environment variable {var} ({} API key) is not set",
                                kind.as_str()
                            )));
                        }
                    } else if kind.requires_api_key() {
                        return Err(Error::Config(format!(
                            "Provider {} requires api_key_env",
                            kind.as_str()
                        )));
                    }
                }
            }
        }
        Ok(credential)
    }

    /// Describe effective routes without credentials or events (oracle
    /// `request_snapshot`).
    pub fn request_snapshot(&self, operation: &str) -> Result<serde_json::Value> {
        require_operation(operation)?;
        let primary = self.routes.get(operation).expect("resolved route").clone();
        let mut routes = vec![primary.clone()];
        for profile in &primary.fallbacks {
            routes.push(model_route(
                &self.config,
                operation,
                profile,
                "explicit fallback".to_string(),
                None,
                Vec::new(),
                None,
            )?);
        }
        let mut connections = serde_json::Map::new();
        let mut protocols = serde_json::Map::new();
        for route in &routes {
            let connection = &self.config.providers[&route.provider];
            let kind = ProviderKind::require(&connection.kind)?;
            let mut metadata = serde_json::to_value(connection).expect("connection JSON");
            metadata["base_url"] = route
                .endpoint
                .clone()
                .map(serde_json::Value::String)
                .unwrap_or(Value::Null);
            metadata["api_key_env"] = connection
                .api_key_env
                .clone()
                .or_else(|| kind.default_api_key_env().map(str::to_string))
                .map(serde_json::Value::String)
                .unwrap_or(Value::Null);
            connections.insert(route.provider.clone(), metadata);
            protocols.insert(
                route.provider.clone(),
                serde_json::Value::from(kind.protocol_version()),
            );
        }
        Ok(serde_json::json!({
            "operation": operation,
            "primary": primary,
            "fallbacks": routes[1..],
            "connections": connections,
            "adapter_protocols": protocols,
        }))
    }

    /// Complete one operation with failover (oracle `complete`).
    pub async fn complete(
        &self,
        messages: &[Message],
        operation: &str,
        json_mode: bool,
        max_tokens: Option<i64>,
    ) -> Result<String> {
        require_operation(operation)?;
        let base = self.routes.get(operation).expect("resolved route").clone();
        let primary = match max_tokens {
            Some(hint) => model_route(
                &self.config,
                operation,
                &base.profile,
                base.origin.clone(),
                base.tier.clone(),
                base.fallbacks.clone(),
                Some(hint),
            )?,
            None => base,
        };
        let mut routes = vec![primary.clone()];
        for profile in &primary.fallbacks {
            routes.push(model_route(
                &self.config,
                operation,
                profile,
                "explicit fallback".to_string(),
                None,
                Vec::new(),
                max_tokens,
            )?);
        }
        // Estimate once in TOKENS: messages are frozen for this call.
        let message_tokens = messages
            .iter()
            .map(|m| count_tokens(&m.content) as i64)
            .sum::<i64>()
            + 4 * messages.len() as i64
            + 8;
        let call_id = uuid_hex();
        let attempt_number = std::sync::atomic::AtomicU32::new(0);
        for (position, route) in routes.iter().enumerate() {
            self.limits.check().map_err(Error::from)?;
            let metadata = self.metadata(&call_id, operation, route);
            self.emit_with(
                &metadata,
                &attempt_number,
                "llm_request_scheduled",
                BTreeMap::new(),
            );
            let input_check = (
                route.max_input_tokens,
                message_tokens,
                operation.to_string(),
                route.profile.clone(),
                route.model.clone(),
            );
            let estimate = message_tokens + route.max_output_tokens.unwrap_or(0);
            let connection_name = route.provider.clone();
            // Input-cap preflight (oracle `attempt_scope` head).
            if let (Some(cap), tokens) = (input_check.0, input_check.1) {
                if tokens > cap {
                    return Err(Error::Config(format!(
                        "{}: estimated {tokens} input tokens exceed max_input_tokens={cap} for model '{}' ({}); \
                         shrink the batch or raise the model limit",
                        input_check.2, input_check.3, input_check.4,
                    )));
                }
            }
            let context = self.request_context(route, &metadata)?;
            let result = self
                .generate_once(
                    messages,
                    route,
                    json_mode,
                    &context,
                    &connection_name,
                    estimate,
                )
                .await;
            match result {
                Ok(text) => {
                    self.emit_with(
                        &metadata,
                        &attempt_number,
                        "llm_request_completed",
                        BTreeMap::new(),
                    );
                    return Ok(text);
                }
                Err(error) => {
                    let failure = ProviderFailure::describe(&error);
                    let mut fields = BTreeMap::new();
                    fields.insert("error_category".to_string(), failure.category.to_string());
                    fields.insert("error_message".to_string(), failure.message.to_string());
                    if let Some(status) = failure.status_code {
                        fields.insert("status_code".to_string(), status.to_string());
                    }
                    self.emit_with(&metadata, &attempt_number, "llm_request_failed", fields);
                    let retryable = error.retry_reason(Some(operation)).is_some();
                    if position + 1 >= routes.len() || !retryable {
                        if self.credentials.is_none() {
                            return Err(Error::Store(format!(
                                "provider {} operation {operation}: {} ({})",
                                route.provider_kind, failure.message, failure.category,
                            )));
                        }
                        return Err(safe_error(&error, &route.provider_kind, operation));
                    }
                    let mut fields = BTreeMap::new();
                    fields.insert(
                        "next_profile".to_string(),
                        routes[position + 1].profile.clone(),
                    );
                    self.emit_with(&metadata, &attempt_number, "llm_model_failover", fields);
                }
            }
        }
        Err(Error::Store("No model route was selected".to_string()))
    }

    fn metadata(
        &self,
        call_id: &str,
        operation: &str,
        route: &ResolvedRoute,
    ) -> BTreeMap<String, String> {
        let mut metadata = BTreeMap::new();
        metadata.insert("call_id".to_string(), call_id.to_string());
        metadata.insert("operation".to_string(), operation.to_string());
        metadata.insert("stage".to_string(), operation.to_string());
        metadata.insert(
            "tier".to_string(),
            route.tier.clone().unwrap_or_else(|| "direct".to_string()),
        );
        metadata.insert("profile".to_string(), route.profile.clone());
        metadata.insert("connection".to_string(), route.provider.clone());
        metadata.insert("provider".to_string(), route.provider_kind.clone());
        metadata.insert("model".to_string(), route.model.clone());
        metadata.insert(
            "inference_fingerprint".to_string(),
            route.fingerprint.clone(),
        );
        metadata.insert(
            "max_output_tokens".to_string(),
            route
                .max_output_tokens
                .map(|n| n.to_string())
                .unwrap_or_default(),
        );
        metadata.insert("adapter_protocol".to_string(), "1".to_string());
        metadata
    }

    fn emit_with(
        &self,
        metadata: &BTreeMap<String, String>,
        attempt_number: &std::sync::atomic::AtomicU32,
        event: &str,
        mut payload: BTreeMap<String, String>,
    ) {
        let mut data = metadata.clone();
        data.insert(
            "attempt".to_string(),
            attempt_number
                .load(std::sync::atomic::Ordering::SeqCst)
                .to_string(),
        );
        data.append(&mut payload);
        let data = match &self.redact {
            Some(redact) => data
                .into_iter()
                .map(|(k, v)| (k, redact.redact(&v)))
                .collect(),
            None => data,
        };
        (self.emit)(event, &data);
    }

    #[allow(clippy::too_many_arguments)]
    fn request_context(
        &self,
        route: &ResolvedRoute,
        metadata: &BTreeMap<String, String>,
    ) -> Result<RequestContext> {
        let metadata = metadata.clone();
        let emit = self.emit.clone();
        let redact = self.redact.clone();
        let emit_fn: Emit = Arc::new(move |event, payload| {
            let mut data = metadata.clone();
            for (k, v) in payload {
                data.insert(k.clone(), v.clone());
            }
            let data = match &redact {
                Some(redact) => data
                    .into_iter()
                    .map(|(k, v)| (k, redact.redact(&v)))
                    .collect(),
                None => data,
            };
            emit(event, &data);
        });
        let tracker = Arc::clone(&self.usage);
        let attribution = UsageAttribution {
            tier: route.tier.clone().unwrap_or_else(|| "direct".to_string()),
            stage: route.operation.clone(),
            provider: route.provider_identity.clone(),
            model: route.model_identity.clone(),
        };
        let labels: BTreeMap<String, String> = [
            (
                route.provider_identity.clone(),
                format!(
                    "{} {}",
                    route.provider_kind,
                    route.endpoint.clone().unwrap_or_default()
                )
                .trim()
                .to_string(),
            ),
            (
                route.model_identity.clone(),
                format!("{} / {}", route.provider_kind, route.model),
            ),
        ]
        .into_iter()
        .collect();
        let usage = Arc::new(move |sample: Option<UsageSample>, elapsed_ms: i64| {
            let sample = sample.map(|mut s| {
                if elapsed_ms > 0 {
                    s.completion_ms = elapsed_ms.max(0);
                }
                s
            });
            tracker.record(sample, &attribution, &labels);
        });
        Ok(RequestContext {
            operation: route.operation.clone(),
            tier: route.tier.clone().unwrap_or_else(|| "direct".to_string()),
            max_tokens: route.max_output_tokens,
            emit: emit_fn,
            usage,
            redact: self.redact.clone(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn generate_once(
        &self,
        messages: &[Message],
        route: &ResolvedRoute,
        json_mode: bool,
        context: &RequestContext,
        _connection_name: &str,
        estimate: i64,
    ) -> Result<String, ProviderError> {
        // Fake connections run through the same retry path without HTTP.
        if route.provider_kind == "fake" {
            let fake = FakeProvider::new();
            let model = ResolvedModel {
                model: route.model.clone(),
                options: serde_json::from_str(&route.options_json).unwrap_or(Value::Null),
                connection_options: Value::Null,
            };
            let config = HttpConfig {
                timeout: std::time::Duration::from_secs(30),
                connect_timeout: std::time::Duration::from_secs(5),
                max_retries: 0,
                endpoint: None,
            };
            let attribution = UsageAttribution {
                tier: route.tier.clone().unwrap_or_else(|| "direct".to_string()),
                stage: route.operation.clone(),
                provider: route.provider_identity.clone(),
                model: route.model_identity.clone(),
            };
            return generate_with_retry(
                &fake,
                &config,
                messages,
                &model,
                json_mode,
                context,
                &self.usage,
                &attribution,
                &BTreeMap::new(),
                &route.operation,
            )
            .await;
        }
        let adapter = self.adapter_for(route)?;
        adapter
            .execute(messages, route, json_mode, context, &self.usage, estimate)
            .await
    }

    fn adapter_for(&self, route: &ResolvedRoute) -> Result<LiveAdapter<'_>, ProviderError> {
        let mut connections = self.connections.lock().expect("connections lock");
        if !connections.contains_key(&route.provider) {
            let cfg = self.config.providers.get(&route.provider).ok_or_else(|| {
                ProviderError::transport(
                    crate::llm::errors::ProviderErrorKind::Unknown,
                    format!("Unknown connection {:?}", route.provider),
                )
            })?;
            let kind = ProviderKind::require(&cfg.kind).map_err(|e| {
                ProviderError::transport(
                    crate::llm::errors::ProviderErrorKind::Unknown,
                    e.to_string(),
                )
            })?;
            let endpoint = cfg.base_url.clone().or_else(|| kind.default_base_url());
            let http_config = HttpConfig::from_connection(cfg, endpoint.clone());
            let client = http_client(&http_config)?;
            let credential = self.credential_for(&route.provider, false).map_err(|e| {
                ProviderError::transport(
                    crate::llm::errors::ProviderErrorKind::Unknown,
                    e.to_string(),
                )
            })?;
            connections.insert(
                route.provider.clone(),
                Connection {
                    kind,
                    config: http_config,
                    endpoint,
                    credential,
                    client,
                },
            );
        }
        let connection = connections
            .get(&route.provider)
            .expect("inserted connection");
        Ok(LiveAdapter {
            kind: connection.kind,
            endpoint: connection.endpoint.clone(),
            credential: connection.credential.clone(),
            client: connection.client.clone(),
            http_config: connection.config.clone(),
            connection_name: route.provider.clone(),
            limits: &self.limits,
            emit: self.emit.clone(),
        })
    }
}

use serde_json::Value;

/// One live HTTP attempt cycle for a route.
struct LiveAdapter<'a> {
    kind: ProviderKind,
    endpoint: Option<String>,
    credential: Option<String>,
    client: reqwest::Client,
    http_config: HttpConfig,
    connection_name: String,
    limits: &'a RequestLimits,
    emit: Emit,
}

impl LiveAdapter<'_> {
    async fn execute(
        &self,
        messages: &[Message],
        route: &ResolvedRoute,
        json_mode: bool,
        context: &RequestContext,
        tracker: &UsageTracker,
        estimate: i64,
    ) -> Result<String, ProviderError> {
        let max_attempts = self.http_config.max_retries + 1;
        let local = is_loopback(self.endpoint.as_deref());
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            // Permit per attempt (never held across backoff).
            let permit = self
                .limits
                .acquire(&self.connection_name, estimate, &self.emit)
                .map_err(|stopped| {
                    ProviderError::transport(
                        crate::llm::errors::ProviderErrorKind::Unknown,
                        stopped.message,
                    )
                })?;
            let started = Instant::now();
            let outcome = self.attempt(messages, route, json_mode, context).await;
            let elapsed_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
            match outcome {
                Ok((text, sample)) => {
                    if let Some(sample) = sample {
                        permit.settle(sample.total_tokens);
                    }
                    // Single recording path: the context hook writes the
                    // ledger, trues up nothing else, and emits llm_usage.
                    (context.usage)(sample, elapsed_ms);
                    let _ = tracker;
                    return Ok(text);
                }
                Err(error) => {
                    let reason = error.retry_reason(Some(&route.operation));
                    let retryable = match reason {
                        None => false,
                        Some("connection") if local && error.status.is_none() => false,
                        Some(_) => true,
                    };
                    drop(permit);
                    if !retryable || attempt >= max_attempts.max(1) {
                        return Err(error);
                    }
                    let wait = error
                        .retry_after_seconds()
                        .map(std::time::Duration::from_secs_f64)
                        .unwrap_or_else(|| backoff(attempt));
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }

    async fn attempt(
        &self,
        messages: &[Message],
        route: &ResolvedRoute,
        json_mode: bool,
        _context: &RequestContext,
    ) -> Result<(String, Option<UsageSample>), ProviderError> {
        if self.kind == ProviderKind::Gemini {
            return self.attempt_gemini(messages, route, json_mode).await;
        }
        let options: Value = serde_json::from_str(&route.options_json).unwrap_or(Value::Null);
        let connection_options = self.connection_options(route);
        let body = crate::llm::compat::build_request_body(
            self.kind,
            &route.model,
            &options,
            &connection_options,
            messages,
            json_mode,
            route.max_output_tokens,
        );
        let endpoint = self.endpoint.clone().ok_or_else(|| {
            ProviderError::transport(
                crate::llm::errors::ProviderErrorKind::Unknown,
                format!("Provider {} requires base_url", self.kind.as_str()),
            )
        })?;
        let url = format!("{}/chat/completions", endpoint.trim_end_matches('/'));
        let mut request = self.client.post(&url).json(&body);
        if let Some(key) = &self.credential {
            request = request.bearer_auth(key);
        }
        if self.kind == ProviderKind::OpencodeGo {
            request = request
                .header("User-Agent", "wenyi")
                .header("x-opencode-session", uuid_hex());
        }
        // Fake-HTTP tests point the endpoint at a local stub server.
        let response = request.send().await.map_err(classify_reqwest)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            // Drain the body so the connection can be reused; never log it.
            let _ = response.bytes().await;
            return Err(ProviderError::http(status, &headers, None));
        }
        let payload: Value = response.json().await.map_err(|_| {
            ProviderError::transport(
                crate::llm::errors::ProviderErrorKind::Unknown,
                "invalid provider response",
            )
        })?;
        let fallback = self.json_fallback(route, &payload, json_mode);
        let completion = crate::llm::compat::extract_completion(
            self.kind.as_str(),
            &payload,
            json_mode,
            fallback.as_deref(),
        )?;
        let sample = completion.usage.as_ref().and_then(|usage| {
            if self.kind == ProviderKind::Deepseek {
                crate::llm::compat::normalize_deepseek_usage(Some(usage))
            } else {
                crate::llm::compat::normalize_openai_usage(Some(usage))
            }
        });
        Ok((completion.text, sample))
    }

    fn connection_options(&self, route: &ResolvedRoute) -> Value {
        serde_json::from_str(&route.connection_options_json).unwrap_or(Value::Null)
    }

    fn json_fallback(
        &self,
        route: &ResolvedRoute,
        payload: &Value,
        json_mode: bool,
    ) -> Option<String> {
        if !json_mode {
            return None;
        }
        let options: Value = serde_json::from_str(&route.options_json).unwrap_or(Value::Null);
        let gate = match self.kind {
            ProviderKind::OpencodeGo => true,
            _ => {
                options
                    .get("json_response_fallback")
                    .and_then(|v| v.as_str())
                    == Some("reasoning_content")
            }
        };
        if !gate {
            return None;
        }
        payload
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("reasoning_content"))
            .and_then(|r| r.as_str())
            .map(str::to_string)
    }

    async fn attempt_gemini(
        &self,
        messages: &[Message],
        route: &ResolvedRoute,
        json_mode: bool,
    ) -> Result<(String, Option<UsageSample>), ProviderError> {
        use crate::llm::errors::ProviderErrorKind;
        let options: Value = serde_json::from_str(&route.options_json).unwrap_or(Value::Null);
        let (system, contents) = gemini_contents(messages);
        let mut generation_config = serde_json::Map::new();
        if json_mode {
            generation_config.insert(
                "responseMimeType".to_string(),
                Value::String("application/json".to_string()),
            );
        }
        if let Some(max_tokens) = route.max_output_tokens {
            generation_config.insert("maxOutputTokens".to_string(), Value::from(max_tokens));
        }
        if let Some(temperature) = options.get("temperature").and_then(|v| v.as_f64()) {
            generation_config.insert("temperature".to_string(), Value::from(temperature));
        }
        if options
            .get("thinking_level")
            .and_then(|v| v.as_str())
            .is_some()
            || options
                .get("thinking_budget")
                .and_then(|v| v.as_i64())
                .is_some()
        {
            let mut thinking = serde_json::Map::new();
            if let Some(level) = options.get("thinking_level").and_then(|v| v.as_str()) {
                thinking.insert(
                    "thinkingLevel".to_string(),
                    Value::String(level.to_string()),
                );
            }
            if let Some(budget) = options.get("thinking_budget").and_then(|v| v.as_i64()) {
                thinking.insert("thinkingBudget".to_string(), Value::from(budget));
            }
            generation_config.insert("thinkingConfig".to_string(), Value::Object(thinking));
        }
        if let Some(extra) = options.get("extra_body").and_then(|v| v.as_object()) {
            for (k, v) in extra {
                generation_config.insert(k.clone(), v.clone());
            }
        }
        let mut body = serde_json::json!({
            "contents": contents,
            "generationConfig": generation_config,
        });
        if let Some(system) = system {
            body["systemInstruction"] = serde_json::json!({"parts": [{"text": system}]});
        }
        let endpoint = self
            .endpoint
            .clone()
            .unwrap_or_else(|| "https://generativelanguage.googleapis.com".to_string());
        let url = format!(
            "{}/v1beta/models/{}:generateContent",
            endpoint.trim_end_matches('/'),
            route.model
        );
        let key = self.credential.clone().unwrap_or_default();
        let response = self
            .client
            .post(&url)
            .header("x-goog-api-key", key)
            .json(&body)
            .send()
            .await
            .map_err(classify_reqwest)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            let _ = response.bytes().await;
            return Err(ProviderError::http(status, &headers, None));
        }
        let payload: Value = response.json().await.map_err(|_| {
            ProviderError::transport(ProviderErrorKind::Unknown, "invalid provider response")
        })?;
        let candidate = payload
            .get("candidates")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first());
        let finish = candidate
            .and_then(|c| c.get("finishReason"))
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_uppercase();
        if finish.contains("MAX_TOKENS") {
            return Err(ProviderError::transport(
                ProviderErrorKind::TruncatedResponse,
                "Gemini response was truncated at the token limit",
            ));
        }
        if finish.contains("SAFETY") || finish.contains("BLOCK") {
            return Err(ProviderError::transport(
                ProviderErrorKind::ContentBlocked,
                format!("Gemini API blocked the response (finish_reason={finish})"),
            ));
        }
        let text = candidate
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("text"))
                    .filter_map(|t| t.as_str())
                    .collect::<String>()
            })
            .unwrap_or_default();
        if text.trim().is_empty() {
            return Err(ProviderError::transport(
                ProviderErrorKind::EmptyResponse,
                "Gemini response content is empty",
            ));
        }
        let sample = payload
            .get("usageMetadata")
            .and_then(|u| u.as_object())
            .map(|usage| {
                let prompt = crate::llm::usage::read_usage_int(usage.get("promptTokenCount"));
                let completion =
                    crate::llm::usage::read_usage_int(usage.get("candidatesTokenCount"))
                        + crate::llm::usage::read_usage_int(usage.get("thoughtsTokenCount"));
                let total = {
                    let total = crate::llm::usage::read_usage_int(usage.get("totalTokenCount"));
                    if total != 0 {
                        total
                    } else {
                        prompt + completion
                    }
                };
                let hit = crate::llm::usage::read_usage_int(usage.get("cachedContentTokenCount"));
                UsageSample {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    total_tokens: total,
                    cache_hit_tokens: hit,
                    cache_miss_tokens: (prompt - hit).max(0),
                    completion_ms: 0,
                }
            });
        Ok((text, sample))
    }
}

/// Convert chat messages to Gemini `systemInstruction` + `contents`.
fn gemini_contents(messages: &[Message]) -> (Option<String>, Vec<Value>) {
    let mut system_parts = Vec::new();
    let mut contents = Vec::new();
    for message in messages {
        match message.role.as_str() {
            "system" => {
                if !message.content.trim().is_empty() {
                    system_parts.push(message.content.clone());
                }
            }
            "assistant" => contents.push(serde_json::json!({
                "role": "model",
                "parts": [{"text": message.content}],
            })),
            _ => contents.push(serde_json::json!({
                "role": "user",
                "parts": [{"text": message.content}],
            })),
        }
    }
    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };
    (system, contents)
}

/// Random 128-bit hex without a uuid dependency.
fn uuid_hex() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let state = RandomState::new();
    let mut a = state.build_hasher();
    a.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    let mut b = state.build_hasher();
    b.write_usize(std::process::id() as usize);
    format!("{:016x}{:016x}", a.finish(), b.finish())
}
