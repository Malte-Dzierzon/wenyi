//! Provider transport: one async trait, shared retry, redacted events.
//! Oracle: `llm/transport.py` + `llm/base.py`.
//!
//! Single-owner retry lives here: server `Retry-After` first, else
//! exponential backoff with jitter (capped at 30 s). Adapters implement
//! one `request` call and never retry. Loopback connection failures fail
//! fast (a down local daemon recovers by restart, not backoff).

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::config::ProviderConfig;
use crate::llm::compat::Message;
use crate::llm::errors::{ProviderError, ProviderErrorKind, ProviderFailure, MAX_WAIT_SECONDS};
use crate::llm::providers::ProviderKind;
use crate::llm::usage::{UsageAttribution, UsageSample, UsageTracker};
use crate::redact::Redactor;
use crate::{Error, Result};

/// One event emission (name + redacted fields).
pub type Emit = Arc<dyn Fn(&str, &BTreeMap<String, String>) + Send + Sync>;

/// No-op emitter for offline paths.
pub fn silent_emit() -> Emit {
    Arc::new(|_, _| {})
}

/// Resolved model + validated options for one request.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// Model identifier.
    pub model: String,
    /// Canonical validated options JSON.
    pub options: serde_json::Value,
    /// Canonical validated connection options JSON.
    pub connection_options: serde_json::Value,
}

/// Per-call hooks; shared adapters never retain mutable request context.
pub struct RequestContext {
    /// Operation ID (retry stage).
    pub operation: String,
    /// Routing tier.
    pub tier: String,
    /// Effective output cap.
    pub max_tokens: Option<i64>,
    /// Event emitter.
    pub emit: Emit,
    /// Usage recorder.
    pub usage: UsageRecorder,
    /// Diagnostic redactor.
    pub redact: Option<Redactor>,
}

/// Usage recording callback.
pub type UsageRecorder = Arc<dyn Fn(Option<UsageSample>, i64) + Send + Sync>;
/// Boxed futures keep `dyn Provider` object-safe without an async-trait dep.
pub trait Provider: Send + Sync {
    /// Provider kind.
    fn kind(&self) -> ProviderKind;
    /// Effective endpoint.
    fn endpoint(&self) -> Option<&str>;
    /// Validate credentials without network I/O (env presence / snapshot).
    fn validate_credentials(&self) -> Result<()>;
    /// One guarded transport attempt (no retry inside).
    fn request<'a>(
        &'a self,
        messages: &'a [Message],
        model: &'a ResolvedModel,
        json_mode: bool,
        context: &'a RequestContext,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<String, ProviderError>> + Send + 'a>>;
    /// Models the endpoint offers (`GET /models` or equivalent).
    fn list_models<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<String>, ProviderError>> + Send + 'a>>
    {
        let kind = self.kind().as_str();
        Box::pin(async move {
            Err(ProviderError::transport(
                ProviderErrorKind::Unknown,
                format!("Provider {kind} cannot list models"),
            ))
        })
    }
}
/// Shared HTTP client configuration per connection.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Read timeout (generation budget).
    pub timeout: Duration,
    /// Connect timeout (fail fast on DNS/TLS).
    pub connect_timeout: Duration,
    /// Max attempts after the first.
    pub max_retries: u32,
    /// Effective endpoint (drives loopback fail-fast).
    pub endpoint: Option<String>,
}

impl HttpConfig {
    /// Build from a validated connection envelope.
    pub fn from_connection(connection: &ProviderConfig, endpoint: Option<String>) -> Self {
        Self {
            timeout: Duration::from_secs_f64(connection.timeout.max(0.001)),
            connect_timeout: Duration::from_secs_f64(connection.connect_timeout.max(0.001)),
            max_retries: connection.max_retries.max(0) as u32,
            endpoint,
        }
    }
}

/// Build a shared `reqwest` client: split connect/read timeouts, no SDK
/// retries (this module owns backoff), rustls only.
pub fn http_client(config: &HttpConfig) -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .connect_timeout(config.connect_timeout)
        .timeout(config.timeout)
        .build()
        .map_err(|e| {
            ProviderError::transport(
                ProviderErrorKind::Permanent,
                format!("invalid HTTP client configuration: {e}"),
            )
        })
}

/// True for localhost URLs whose connection failures fail fast.
pub fn is_loopback(base_url: Option<&str>) -> bool {
    let Some(url) = base_url else { return false };
    let authority = url.split("://").nth(1).unwrap_or(url);
    let authority = authority.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(bracketed) = host.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else if host.matches(':').count() > 1 {
        host // Bare IPv6 (port would be ambiguous); compare whole.
    } else {
        host.split(':').next().unwrap_or("")
    }
    .to_lowercase();
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host.ends_with(".localhost")
}

/// Classify a `reqwest` error into the provider taxonomy (no body/secret).
pub fn classify_reqwest(error: reqwest::Error) -> ProviderError {
    if error.is_timeout() {
        return ProviderError::transport(ProviderErrorKind::Timeout, "request timed out");
    }
    if error.is_connect() {
        return ProviderError::transport(ProviderErrorKind::Connection, "connection failed");
    }
    if error.is_builder() {
        return ProviderError::transport(ProviderErrorKind::Permanent, "invalid request");
    }
    ProviderError::transport(ProviderErrorKind::Connection, "transport error")
}

/// Jittered exponential backoff: `min(30, random(0, 2^attempt))`
/// (oracle `wait_random_exponential(multiplier=1, max=30)`).
pub fn backoff(attempt: u32) -> Duration {
    // Deterministic-friendly: hash attempt + nanos for jitter without rand.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let mut seed = (attempt as u64)
        .wrapping_mul(0x9E3779B97F4A7C15)
        .wrapping_add(nanos);
    seed ^= seed >> 30;
    seed = seed.wrapping_mul(0xBF58476D1CE4E5B9);
    let unit = (seed >> 11) as f64 / ((1u64 << 53) as f64);
    let cap = (1u64 << attempt.min(5)) as f64;
    Duration::from_secs_f64((unit * cap).min(MAX_WAIT_SECONDS))
}

/// Run one `generate` with shared retry (oracle `ProviderAdapter.generate`).
/// Returns model text or the last classified error.
#[allow(clippy::too_many_arguments)] // Mirrors the oracle's 10-field RequestContext call shape.
pub async fn generate_with_retry(
    provider: &dyn Provider,
    config: &HttpConfig,
    messages: &[Message],
    model: &ResolvedModel,
    json_mode: bool,
    context: &RequestContext,
    tracker: &UsageTracker,
    attribution: &UsageAttribution,
    labels: &BTreeMap<String, String>,
    operation_for_retry: &str,
) -> Result<String, ProviderError> {
    let max_attempts = config.max_retries + 1;
    let local = is_loopback(config.endpoint.as_deref());
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        // Per-attempt usage capture: the adapter records through the hook.
        let attempt_tracker = AttemptUsage::default();
        let hook_tracker = attempt_tracker.clone();
        let hook_labels = labels.clone();
        let hook_attribution = attribution.clone();
        let attempt_context = RequestContext {
            operation: context.operation.clone(),
            tier: context.tier.clone(),
            max_tokens: context.max_tokens,
            emit: context.emit.clone(),
            usage: Arc::new(move |sample, elapsed_ms| {
                hook_tracker.record(sample, elapsed_ms, &hook_attribution, &hook_labels);
            }),
            redact: context.redact.clone(),
        };
        match provider
            .request(messages, model, json_mode, &attempt_context)
            .await
        {
            Ok(text) => {
                attempt_tracker.flush(tracker);
                return Ok(text);
            }
            Err(error) => {
                attempt_tracker.flush(tracker);
                let reason = error.retry_reason(Some(operation_for_retry));
                let retryable = match reason {
                    None => false,
                    Some("connection") if local && error.status.is_none() => false,
                    Some(_) => true,
                };
                if !retryable || attempt >= max_attempts.max(1) {
                    if attempt >= max_attempts.max(1) && retryable {
                        emit_retry(
                            context,
                            "llm_retry_exhausted",
                            &error,
                            attempt,
                            max_attempts,
                            None,
                        );
                    }
                    return Err(error);
                }
                let wait = error
                    .retry_after_seconds()
                    .map(Duration::from_secs_f64)
                    .unwrap_or_else(|| backoff(attempt));
                let source = if error.retry_after_seconds().is_some() {
                    "server"
                } else {
                    "exponential_jitter"
                };
                emit_retry(
                    context,
                    "llm_retry_wait",
                    &error,
                    attempt,
                    max_attempts,
                    Some((wait, source)),
                );
                tokio::time::sleep(wait).await;
            }
        }
    }
}

fn emit_retry(
    context: &RequestContext,
    event: &str,
    error: &ProviderError,
    attempt: u32,
    max_attempts: u32,
    wait: Option<(Duration, &str)>,
) {
    let failure = ProviderFailure::describe(error);
    let mut fields = BTreeMap::new();
    fields.insert(
        "reason".to_string(),
        error
            .retry_reason(Some(&context.operation))
            .unwrap_or("not_retryable")
            .to_string(),
    );
    fields.insert("error_category".to_string(), failure.category.to_string());
    if let Some(status) = failure.status_code {
        fields.insert("status_code".to_string(), status.to_string());
    }
    if let Some(id) = &error.request_id {
        fields.insert("request_id".to_string(), id.clone());
    }
    fields.insert("attempt".to_string(), attempt.to_string());
    fields.insert("max_attempts".to_string(), max_attempts.to_string());
    if let Some((wait, source)) = wait {
        fields.insert(
            "wait_seconds".to_string(),
            format!("{:.3}", wait.as_secs_f64()),
        );
        fields.insert("wait_source".to_string(), source.to_string());
    }
    let fields = match &context.redact {
        Some(redact) => fields
            .into_iter()
            .map(|(k, v)| (k, redact.redact(&v)))
            .collect(),
        None => fields,
    };
    (context.emit)(event, &fields);
}

/// One staged usage observation (sample, elapsed ms, attribution, labels).
type StagedUsage = (
    Option<UsageSample>,
    i64,
    UsageAttribution,
    BTreeMap<String, String>,
);

/// Per-attempt usage staging (flushed once per attempt, win or lose).
#[derive(Debug, Default, Clone)]
struct AttemptUsage {
    inner: Arc<std::sync::Mutex<Vec<StagedUsage>>>,
}

impl AttemptUsage {
    fn record(
        &self,
        sample: Option<UsageSample>,
        elapsed_ms: i64,
        attribution: &UsageAttribution,
        labels: &BTreeMap<String, String>,
    ) {
        self.inner.lock().expect("usage staging").push((
            sample,
            elapsed_ms,
            attribution.clone(),
            labels.clone(),
        ));
    }

    fn flush(&self, tracker: &UsageTracker) {
        for (sample, elapsed_ms, attribution, labels) in
            self.inner.lock().expect("usage staging").drain(..)
        {
            let sample = sample.map(|mut s| {
                if elapsed_ms > 0 {
                    s.completion_ms = elapsed_ms.max(0);
                }
                s
            });
            tracker.record(sample, &attribution, &labels);
        }
    }
}

/// Credential-scoped safe error: stable classification only, no SDK
/// context, no secrets (oracle `safe_provider_error`).
pub fn safe_error(error: &ProviderError, provider: &str, operation: &str) -> Error {
    let failure = ProviderFailure::describe(error);
    Error::Provider(format!(
        "{provider} {operation}: {} ({})",
        failure.message, failure.category,
    ))
}
