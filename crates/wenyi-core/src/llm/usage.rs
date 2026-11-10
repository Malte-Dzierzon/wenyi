//! Token usage accounting, deltas, persisted merges. Oracle: `llm/usage.py`.
//!
//! One ledger with four independent attribution views (`by_tier`,
//! `by_stage`, `by_provider`, `by_model`); totals always derive from
//! `by_tier` so views are never double-counted.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{Map, Value};

/// Ledger fields in canonical order.
pub const USAGE_FIELDS: [&str; 9] = [
    "calls",
    "prompt_tokens",
    "completion_tokens",
    "total_tokens",
    "cache_hit_tokens",
    "cache_miss_tokens",
    "completion_ms",
    "timed_calls",
    "timed_completion_tokens",
];

/// Attribution views in canonical order.
pub const USAGE_GROUPS: [&str; 4] = ["by_tier", "by_stage", "by_provider", "by_model"];

/// Current ledger schema version.
pub const USAGE_SCHEMA_VERSION: i64 = 2;

/// Normalized provider usage for one call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageSample {
    /// Input tokens billed.
    pub prompt_tokens: i64,
    /// Output tokens billed.
    pub completion_tokens: i64,
    /// Total tokens billed.
    pub total_tokens: i64,
    /// Prompt tokens served from cache.
    pub cache_hit_tokens: i64,
    /// Prompt tokens not served from cache.
    pub cache_miss_tokens: i64,
    /// Wall-clock milliseconds of the provider call.
    pub completion_ms: i64,
}

/// Read an integer usage field; zero for missing/nonnumeric values.
pub fn read_usage_int(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// Build a provider-independent usage record from common API token fields.
pub fn make_usage_sample(
    usage: &Map<String, Value>,
    cache_hit_tokens: i64,
    cache_miss_tokens: i64,
) -> UsageSample {
    let prompt_tokens = read_usage_int(usage.get("prompt_tokens"));
    let completion_tokens = read_usage_int(usage.get("completion_tokens"));
    let total_tokens = {
        let total = read_usage_int(usage.get("total_tokens"));
        if total != 0 {
            total
        } else {
            prompt_tokens + completion_tokens
        }
    };
    UsageSample {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        cache_hit_tokens: cache_hit_tokens.max(0),
        cache_miss_tokens: cache_miss_tokens.max(0),
        completion_ms: 0,
    }
}

fn hit_rate(hit: i64, miss: i64) -> f64 {
    let total = hit + miss;
    if total == 0 {
        0.0
    } else {
        // Oracle `round(hit / total, 4)`.
        ((hit as f64 / total as f64) * 10_000.0).round() / 10_000.0
    }
}

fn empty_slot() -> Map<String, Value> {
    USAGE_FIELDS
        .iter()
        .map(|f| (f.to_string(), Value::from(0)))
        .collect()
}

fn normalize_group(group: &Map<String, Value>) -> Map<String, Value> {
    let mut normalized = Map::new();
    for (name, values) in group {
        let values = values.as_object();
        let mut slot = empty_slot();
        for field in USAGE_FIELDS {
            let value = values.and_then(|v| v.get(field));
            slot[field] = Value::from(read_usage_int(value));
        }
        let hit = read_usage_int(slot.get("cache_hit_tokens"));
        let miss = read_usage_int(slot.get("cache_miss_tokens"));
        slot.insert(
            "cache_hit_rate".to_string(),
            serde_json::json!(hit_rate(hit, miss)),
        );
        normalized.insert(name.clone(), Value::Object(slot));
    }
    normalized
}

fn usage_summary(
    groups: &BTreeMap<String, Map<String, Value>>,
    labels: &BTreeMap<String, String>,
) -> Map<String, Value> {
    let mut summary = Map::new();
    summary.insert(
        "schema_version".to_string(),
        Value::from(USAGE_SCHEMA_VERSION),
    );
    let empty = Map::new();
    let normalized_tier = normalize_group(groups.get("by_tier").unwrap_or(&empty));
    let mut totals = empty_slot();
    for slot in normalized_tier.values() {
        let Some(slot) = slot.as_object() else {
            continue;
        };
        for field in USAGE_FIELDS {
            let current = read_usage_int(totals.get(field));
            totals[field] = Value::from(current + read_usage_int(slot.get(field)));
        }
    }
    let hit = read_usage_int(totals.get("cache_hit_tokens"));
    let miss = read_usage_int(totals.get("cache_miss_tokens"));
    totals.insert(
        "cache_hit_rate".to_string(),
        serde_json::json!(hit_rate(hit, miss)),
    );
    summary.insert("totals".to_string(), Value::Object(totals));
    for group in USAGE_GROUPS {
        summary.insert(
            group.to_string(),
            Value::Object(normalize_group(groups.get(group).unwrap_or(&empty))),
        );
    }
    summary.insert(
        "labels".to_string(),
        Value::Object(
            labels
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect(),
        ),
    );
    summary
}

/// Empty ledger summary.
pub fn empty_usage() -> Map<String, Value> {
    usage_summary(&BTreeMap::new(), &BTreeMap::new())
}

/// Validate a persisted ledger (oracle `validate_usage`).
pub fn validate_usage(value: Option<&Value>) -> crate::Result<Map<String, Value>> {
    let Some(ledger) = value.and_then(|v| v.as_object()) else {
        return Ok(empty_usage());
    };
    if ledger.is_empty() {
        return Ok(empty_usage());
    }
    if read_usage_int(ledger.get("schema_version")) != USAGE_SCHEMA_VERSION {
        let by_tier_empty = ledger
            .get("by_tier")
            .and_then(|v| v.as_object())
            .is_none_or(|g| {
                g.values()
                    .all(|s| s.as_object().is_none_or(|s| s.is_empty()))
            });
        let totals_empty = USAGE_FIELDS.iter().all(|f| {
            read_usage_int(
                ledger
                    .get("totals")
                    .and_then(|t| t.as_object())
                    .and_then(|t| t.get(*f)),
            ) == 0
        });
        if by_tier_empty && totals_empty {
            return Ok(empty_usage());
        }
        return Err(crate::Error::Store(
            "Usage ledger needs conversion via the engine migration API".to_string(),
        ));
    }
    for group in USAGE_GROUPS {
        let Some(slots) = ledger.get(group).and_then(|v| v.as_object()) else {
            return Err(crate::Error::Store(
                "Invalid usage ledger grouping".to_string(),
            ));
        };
        if !slots.values().all(|v| v.is_object()) {
            return Err(crate::Error::Store(
                "Invalid usage ledger grouping".to_string(),
            ));
        }
    }
    Ok(ledger.clone())
}

/// Nonnegative per-slot delta between cumulative ledgers (oracle `usage_delta`).
pub fn usage_delta(
    current: &Map<String, Value>,
    previous: &Map<String, Value>,
) -> Map<String, Value> {
    let mut groups = BTreeMap::new();
    for group in USAGE_GROUPS {
        let empty = Map::new();
        let current_group = current
            .get(group)
            .and_then(|v| v.as_object())
            .unwrap_or(&empty);
        let previous_group = previous
            .get(group)
            .and_then(|v| v.as_object())
            .unwrap_or(&empty);
        let mut delta = Map::new();
        for (name, values) in current_group {
            let values = values.as_object();
            let old = previous_group.get(name).and_then(|v| v.as_object());
            let mut slot = empty_slot();
            for field in USAGE_FIELDS {
                let now = read_usage_int(values.and_then(|v| v.get(field)));
                let was = read_usage_int(old.and_then(|v| v.get(field)));
                slot[field] = Value::from((now - was).max(0));
            }
            if slot.values().any(|v| read_usage_int(Some(v)) != 0) {
                delta.insert(name.clone(), Value::Object(slot));
            }
        }
        groups.insert(group.to_string(), delta);
    }
    let labels = current
        .get("labels")
        .and_then(|v| v.as_object())
        .map(|labels| {
            labels
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    usage_summary(&groups, &labels)
}

/// Merge one unpersisted increment into cumulative usage (oracle
/// `merge_usage_summaries`).
pub fn merge_usage_summaries(
    accumulated: &Map<String, Value>,
    increment: &Map<String, Value>,
) -> Map<String, Value> {
    let mut groups = BTreeMap::new();
    for group in USAGE_GROUPS {
        let empty = Map::new();
        let base = accumulated
            .get(group)
            .and_then(|v| v.as_object())
            .unwrap_or(&empty);
        let extra = increment
            .get(group)
            .and_then(|v| v.as_object())
            .unwrap_or(&empty);
        let mut merged: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
        for (name, values) in base.iter().chain(extra.iter()) {
            let values = values.as_object();
            let slot = merged.entry(name.clone()).or_insert_with(empty_slot);
            for field in USAGE_FIELDS {
                let current = read_usage_int(slot.get(field));
                slot[field] =
                    Value::from(current + read_usage_int(values.and_then(|v| v.get(field))));
            }
        }
        groups.insert(
            group.to_string(),
            merged
                .into_iter()
                .map(|(k, v)| (k, Value::Object(v)))
                .collect(),
        );
    }
    let mut labels: BTreeMap<String, String> = accumulated
        .get("labels")
        .and_then(|v| v.as_object())
        .map(|labels| {
            labels
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    if let Some(extra) = increment.get("labels").and_then(|v| v.as_object()) {
        for (k, v) in extra {
            if let Some(s) = v.as_str() {
                labels.insert(k.clone(), s.to_string());
            }
        }
    }
    usage_summary(&groups, &labels)
}

/// One thread-safe ledger with independent attribution views.
#[derive(Debug, Default)]
pub struct UsageTracker {
    inner: Mutex<UsageInner>,
}

#[derive(Debug, Default)]
struct UsageInner {
    groups: BTreeMap<String, BTreeMap<String, Map<String, Value>>>,
    labels: BTreeMap<String, String>,
}

/// Attribution keys for one recorded sample.
#[derive(Debug, Clone, Serialize)]
pub struct UsageAttribution {
    /// Routing tier.
    pub tier: String,
    /// Pipeline stage / operation.
    pub stage: String,
    /// Provider connection identity.
    pub provider: String,
    /// Model identity.
    pub model: String,
}

impl UsageTracker {
    /// Empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one sample under all four views.
    pub fn record(
        &self,
        sample: Option<UsageSample>,
        attribution: &UsageAttribution,
        labels: &BTreeMap<String, String>,
    ) {
        let Some(sample) = sample else { return };
        let mut inner = self.inner.lock().expect("usage lock");
        for (group, key) in [
            ("by_tier", &attribution.tier),
            ("by_stage", &attribution.stage),
            ("by_provider", &attribution.provider),
            ("by_model", &attribution.model),
        ] {
            let slot = inner
                .groups
                .entry(group.to_string())
                .or_default()
                .entry(key.clone())
                .or_insert_with(empty_slot);
            let bump = |slot: &mut Map<String, Value>, field: &str, delta: i64| {
                let current = read_usage_int(slot.get(field));
                slot[field] = Value::from(current + delta);
            };
            bump(slot, "calls", 1);
            bump(slot, "prompt_tokens", sample.prompt_tokens);
            bump(slot, "completion_tokens", sample.completion_tokens);
            bump(slot, "total_tokens", sample.total_tokens);
            bump(slot, "cache_hit_tokens", sample.cache_hit_tokens);
            bump(slot, "cache_miss_tokens", sample.cache_miss_tokens);
            bump(slot, "completion_ms", sample.completion_ms);
            if sample.completion_ms > 0 {
                bump(slot, "timed_calls", 1);
                bump(slot, "timed_completion_tokens", sample.completion_tokens);
            }
        }
        inner
            .labels
            .extend(labels.iter().map(|(k, v)| (k.clone(), v.clone())));
    }

    /// Current summary (totals derive from `by_tier`).
    pub fn summary(&self) -> Map<String, Value> {
        let inner = self.inner.lock().expect("usage lock");
        let groups = inner
            .groups
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.iter()
                        .map(|(n, s)| (n.clone(), Value::Object(s.clone())))
                        .collect(),
                )
            })
            .collect();
        usage_summary(&groups, &inner.labels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_ledger_slot_shape_is_store_error() {
        let mut ledger = empty_usage();
        ledger.insert("by_tier".to_string(), serde_json::json!({"strong": 42}));
        let err = validate_usage(Some(&Value::Object(ledger))).unwrap_err();
        assert!(
            err.to_string().contains("Invalid usage ledger grouping"),
            "unexpected error: {err}"
        );
    }
}
