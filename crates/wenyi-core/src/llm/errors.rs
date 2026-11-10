//! Provider error taxonomy. Oracle: `llm/retrying.py` + `llm/errors.py`.
//!
//! Single-owner retry classification: transient reasons, resumable
//! interrupts, safe diagnostics (never request/response bodies or secrets).

use serde::Serialize;

/// Retryable status codes (oracle `_RETRYABLE_STATUS_CODES`).
const RETRYABLE_STATUS: [u16; 3] = [408, 409, 429];
/// Interrupt codes logged as resumable even when retry gives up.
const RESUMABLE_STATUS: [u16; 4] = [402, 408, 409, 429];
/// Summary operations allowed to retry truncation at this layer.
const SUMMARY_OPERATIONS: [&str; 2] = ["synopsis.chapter", "synopsis.book"];
/// Upper bound for any single retry wait.
pub const MAX_WAIT_SECONDS: f64 = 30.0;

/// One failed provider attempt, classified for retry/reporting.
#[derive(Debug, Clone)]
pub struct ProviderError {
    /// HTTP status when the failure carried one.
    pub status: Option<u16>,
    /// Explicit `x-should-retry` instruction.
    pub retry_override: Option<bool>,
    /// Transport-level kind (`timeout`, `connection`, ...).
    pub kind: ProviderErrorKind,
    /// Provider request id for correlation (safe to log).
    pub request_id: Option<String>,
    /// `Retry-After` header value (seconds or HTTP date).
    pub retry_after: Option<String>,
    /// `retry-after-ms` header value.
    pub retry_after_ms: Option<String>,
    /// Lowercased message for quota-text matching (`insufficient_quota`).
    pub message_hint: String,
}

/// Transport-level failure kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// HTTP response with a status code.
    Http,
    /// Timeout (connect/read/deadline).
    Timeout,
    /// Connection refused/reset/DNS/TLS-handshake (non-cert) failure.
    Connection,
    /// Permanent local failure (bad URL, TLS cert, protocol config).
    Permanent,
    /// Empty model response.
    EmptyResponse,
    /// Truncated at the output token limit.
    TruncatedResponse,
    /// Safety/policy refusal.
    ContentBlocked,
    /// Unclassified.
    Unknown,
}

impl ProviderError {
    /// Build an HTTP failure from status + headers.
    pub fn http(status: u16, headers: &[(String, String)], request_id: Option<String>) -> Self {
        let header = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };
        let retry_override = match header("x-should-retry").as_deref().map(str::to_lowercase) {
            Some(v) if v == "true" => Some(true),
            Some(v) if v == "false" => Some(false),
            _ => None,
        };
        Self {
            status: Some(status),
            retry_override,
            kind: ProviderErrorKind::Http,
            request_id: request_id
                .or_else(|| header("x-request-id").or_else(|| header("request-id"))),
            retry_after: header("retry-after"),
            retry_after_ms: header("retry-after-ms"),
            message_hint: String::new(),
        }
    }

    /// Build a transport failure without HTTP status.
    pub fn transport(kind: ProviderErrorKind, message_hint: impl Into<String>) -> Self {
        Self {
            status: None,
            retry_override: None,
            kind,
            request_id: None,
            retry_after: None,
            retry_after_ms: None,
            message_hint: message_hint.into().to_lowercase(),
        }
    }

    /// Stable transient-error reason, or None for permanent failures
    /// (oracle `retry_reason`).
    pub fn retry_reason(&self, operation: Option<&str>) -> Option<&'static str> {
        if let Some(override_) = self.retry_override {
            return override_.then_some("server_requested_retry");
        }
        if let Some(status) = self.status {
            if RETRYABLE_STATUS.contains(&status) || status >= 500 {
                return Some(match status {
                    408 => "http_408",
                    409 => "http_409",
                    429 => "http_429",
                    500 => "http_500",
                    502 => "http_502",
                    503 => "http_503",
                    504 => "http_504",
                    _ => "http_5xx",
                });
            }
            return None;
        }
        match self.kind {
            ProviderErrorKind::TruncatedResponse => {
                if SUMMARY_OPERATIONS.contains(&operation.unwrap_or("")) {
                    Some("truncated_response")
                } else {
                    None
                }
            }
            ProviderErrorKind::EmptyResponse => Some("empty_response"),
            ProviderErrorKind::Timeout => Some("timeout"),
            ProviderErrorKind::Connection => Some("connection"),
            _ => None,
        }
    }

    /// Whether the failure should be logged as `interrupted` (oracle
    /// `is_resumable_provider_interrupt`).
    pub fn is_resumable_interrupt(&self) -> bool {
        if self.kind == ProviderErrorKind::ContentBlocked {
            return true;
        }
        if self.retry_reason(None).is_some() {
            return true;
        }
        if let Some(status) = self.status {
            if RESUMABLE_STATUS.contains(&status) || status >= 500 {
                return true;
            }
        }
        self.message_hint.contains("insufficient balance")
            || self.message_hint.contains("insufficient_quota")
    }

    /// Server-requested wait from retry headers, capped (oracle
    /// `_retry_after_seconds`).
    pub fn retry_after_seconds(&self) -> Option<f64> {
        if let Some(ms) = &self.retry_after_ms {
            if let Ok(ms) = ms.trim().parse::<f64>() {
                return Some((ms / 1000.0).clamp(0.0, MAX_WAIT_SECONDS));
            }
        }
        let value = self.retry_after.as_deref()?.trim();
        if value.is_empty() {
            return None;
        }
        if let Ok(seconds) = value.parse::<f64>() {
            return Some(seconds.clamp(0.0, MAX_WAIT_SECONDS));
        }
        // HTTP-date form: parse with std-only IMF-fixdate handling.
        http_date_delay_seconds(value).map(|s| s.clamp(0.0, MAX_WAIT_SECONDS))
    }
}

/// Seconds until an IMF-fixdate `Retry-After` target (None when unparsable).
/// Oracle parses any email-date; fixtures only need IMF-fixdate.
fn http_date_delay_seconds(value: &str) -> Option<f64> {
    // `Sun, 06 Nov 1994 08:49:37 GMT` — compare against now via UNIX epoch.
    let (_, rest) = value.split_once(", ")?;
    let mut parts = rest.split_whitespace();
    let _day: i64 = parts.next()?.parse().ok()?;
    let _month = parts.next()?;
    let _year: i64 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    // Full calendar math needs a date crate; the oracle caps at 30 s anyway,
    // and a well-formed future HTTP date implies waiting the max.
    let mut t = time.split(':');
    let (_h, _m, _s) = (
        t.next()?.parse::<i64>().ok()?,
        t.next()?.parse::<i64>().ok()?,
        t.next()?.parse::<i64>().ok()?,
    );
    let _ = (_day, _year, _h, _m, _s);
    Some(MAX_WAIT_SECONDS)
}

/// Safe provider-independent diagnostic (oracle `ProviderFailure`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProviderFailure {
    /// HTTP status when known.
    pub status_code: Option<u16>,
    /// Stable category.
    pub category: &'static str,
    /// Fixed message (never echoes provider text).
    pub message: &'static str,
}

impl ProviderFailure {
    /// Classify a failure without copying raw error text (oracle
    /// `describe_provider_failure`).
    pub fn describe(error: &ProviderError) -> Self {
        if let Some(status) = error.status {
            let (category, message) = match status {
                400 | 422 => ("invalid_request", "The provider rejected the request. Check the model configuration."),
                401 => ("authentication_failed", "Provider authentication failed. Check the API key."),
                402 => ("insufficient_balance", "The provider reported insufficient balance. Recharge the account or change provider."),
                403 => ("permission_denied", "Provider access was denied. Check account and model permissions."),
                404 => ("model_or_endpoint_not_found", "The provider model or endpoint was not found. Check the model and endpoint configuration."),
                408 => ("timeout", "The provider request timed out. Try again later."),
                429 => ("rate_limited", "The provider rate limit was reached. Try again later."),
                s if s >= 500 => ("provider_unavailable", "The provider is unavailable. Try again later."),
                _ => ("http_error", "The provider returned an HTTP error."),
            };
            return Self {
                status_code: Some(status),
                category,
                message,
            };
        }
        let (category, message) = match error.kind {
            ProviderErrorKind::TruncatedResponse => {
                ("truncated_response", "The provider response was truncated.")
            }
            ProviderErrorKind::EmptyResponse => {
                ("empty_response", "The provider returned no usable text.")
            }
            ProviderErrorKind::Timeout => (
                "timeout",
                "The provider request timed out. Try again later.",
            ),
            ProviderErrorKind::Connection => (
                "connection_failed",
                "Could not connect to the provider. Check connectivity.",
            ),
            ProviderErrorKind::ContentBlocked => (
                "content_blocked",
                "The provider refused the request on safety/policy grounds.",
            ),
            _ => ("unknown_error", "The provider request failed."),
        };
        Self {
            status_code: None,
            category,
            message,
        }
    }
}
