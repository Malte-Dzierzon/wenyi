//! LLM golden parity against the pinned Python oracle.
//!
//! Expectations in `tests/golden/snapshots/llm.json` were captured offline
//! (`tests/golden/llm_fixtures.py`): operation registry, routing
//! fingerprints, per-kind request bodies, retry classification, usage
//! ledger math, JSON repairs, fake-client call records.

use std::collections::BTreeMap;
use std::path::PathBuf;
use wenyi_core::config;
use wenyi_core::llm::{compat, errors, json, operations, providers, routing, usage};

fn golden() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("golden")
        .join("snapshots")
        .join("llm.json");
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn messages() -> Vec<compat::Message> {
    vec![
        compat::Message {
            role: "system".to_string(),
            content: "You translate.".to_string(),
        },
        compat::Message {
            role: "user".to_string(),
            content: "Hello world".to_string(),
        },
    ]
}

fn deepseek_config() -> config::LlmConfig {
    config::load("llm:\n  preset: deepseek\n").unwrap().llm
}

#[test]
fn operations_match_oracle() {
    let want = golden();
    let mut got: Vec<&str> = operations::operation_ids();
    got.sort_unstable();
    let mut want_ops: Vec<&str> = want["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    want_ops.sort_unstable();
    assert_eq!(got, want_ops);
}

#[test]
fn routing_fingerprints_match_oracle() {
    let want = golden();
    let cfg = deepseek_config();
    let routes = routing::resolve_routes(&cfg).unwrap();
    let ops: Vec<String> = want["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let snapshot = routing::inference_snapshot(&cfg, &ops).unwrap();
    for (operation, want_entry) in want["snapshot"].as_object().unwrap() {
        let got = snapshot
            .get(operation)
            .unwrap_or_else(|| panic!("missing {operation}"));
        assert_eq!(
            got["primary"].as_str().unwrap(),
            want_entry["primary"].as_str().unwrap(),
            "{operation} primary"
        );
        assert_eq!(
            got["fallbacks"], want_entry["fallbacks"],
            "{operation} fallbacks"
        );
    }
    // Full route shape for one operation.
    let route = routes.get("translation.body").unwrap();
    let want_route = &want["route_describe"];
    assert_eq!(route.origin, want_route["origin"].as_str().unwrap());
    assert_eq!(route.tier.as_deref(), want_route["tier"].as_str());
    assert_eq!(route.profile, want_route["profile"].as_str().unwrap());
    assert_eq!(route.provider, want_route["provider"].as_str().unwrap());
    assert_eq!(
        route.provider_kind,
        want_route["provider_kind"].as_str().unwrap()
    );
    assert_eq!(route.endpoint.as_deref(), want_route["endpoint"].as_str());
    assert_eq!(route.model, want_route["model"].as_str().unwrap());
    assert_eq!(
        route.max_output_tokens,
        want_route["max_output_tokens"].as_i64()
    );
    assert_eq!(
        route.provider_identity,
        want_route["provider_identity"].as_str().unwrap()
    );
    assert_eq!(
        route.model_identity,
        want_route["model_identity"].as_str().unwrap()
    );
    assert_eq!(
        route.fingerprint,
        want_route["fingerprint"].as_str().unwrap()
    );
    let got_options: serde_json::Value = serde_json::from_str(&route.options_json).unwrap();
    assert_eq!(got_options, want_route["options"]);
}

#[test]
fn request_bodies_match_oracle() {
    use providers::ProviderKind;
    let want = golden();
    let messages = messages();
    let check = |name: &str,
                 kind: ProviderKind,
                 options: serde_json::Value,
                 conn: serde_json::Value,
                 json_mode: bool,
                 max_tokens: Option<i64>| {
        let got = compat::build_request_body(
            kind, "m", &options, &conn, &messages, json_mode, max_tokens,
        );
        assert_eq!(got, want["bodies"][name], "{name}");
    };
    let thinking =
        serde_json::json!({"thinking": true, "reasoning_effort": "high", "extra_body": {}});
    let no_think =
        serde_json::json!({"thinking": false, "reasoning_effort": "high", "extra_body": {}});
    let compat_opts = serde_json::json!({
        "thinking": false, "reasoning_effort": "high",
        "json_response_fallback": "none", "request_overrides": {},
    });
    let conn_none = serde_json::json!({"reasoning_style": "none"});
    check(
        "deepseek",
        ProviderKind::Deepseek,
        thinking.clone(),
        serde_json::json!({}),
        false,
        Some(100),
    );
    check(
        "deepseek_json",
        ProviderKind::Deepseek,
        thinking.clone(),
        serde_json::json!({}),
        true,
        Some(100),
    );
    check(
        "deepseek_no_think",
        ProviderKind::Deepseek,
        no_think.clone(),
        serde_json::json!({}),
        false,
        None,
    );
    check(
        "openai",
        ProviderKind::Openai,
        thinking.clone(),
        serde_json::json!({}),
        false,
        Some(100),
    );
    check(
        "openrouter",
        ProviderKind::Openrouter,
        thinking.clone(),
        serde_json::json!({}),
        false,
        Some(100),
    );
    let opencode =
        serde_json::json!({"thinking": false, "reasoning_effort": "low", "extra_body": {}});
    check(
        "opencode_go",
        ProviderKind::OpencodeGo,
        opencode,
        serde_json::json!({}),
        false,
        Some(100),
    );
    for style in ["none", "deepseek", "openai", "openrouter"] {
        let conn = serde_json::json!({"reasoning_style": style});
        check(
            &format!("compatible_{style}"),
            ProviderKind::OpenaiCompatible,
            compat_opts.clone(),
            conn,
            false,
            Some(100),
        );
    }
    let _ = conn_none;
    assert_eq!(
        compat::base_request_body("m", &messages, false),
        want["bodies"]["base_plain"]
    );
    assert_eq!(
        compat::base_request_body("m", &messages, true),
        want["bodies"]["base_json"]
    );
}

#[test]
fn retry_classification_matches_oracle() {
    let want = golden();
    for case in want["retry"].as_array().unwrap() {
        if let Some(status) = case.get("status").and_then(|v| v.as_u64()) {
            let error = errors::ProviderError::http(status as u16, &[], None);
            let reason = error
                .retry_reason(Some("translation.body"))
                .map(str::to_string);
            let want_reason = case
                .get("reason")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            assert_eq!(reason, want_reason, "status {status} reason");
            assert_eq!(
                error.is_resumable_interrupt(),
                case["resumable"].as_bool().unwrap(),
                "status {status} resumable"
            );
            let failure = errors::ProviderFailure::describe(&error);
            assert_eq!(
                failure.category,
                case["category"].as_str().unwrap(),
                "status {status}"
            );
        } else {
            let kind = case["kind"].as_str().unwrap();
            let (error, operation) = match kind {
                "empty" => (
                    errors::ProviderError::transport(errors::ProviderErrorKind::EmptyResponse, "x"),
                    "translation.body",
                ),
                "truncated_synopsis" => (
                    errors::ProviderError::transport(
                        errors::ProviderErrorKind::TruncatedResponse,
                        "x",
                    ),
                    "synopsis.chapter",
                ),
                _ => (
                    errors::ProviderError::transport(
                        errors::ProviderErrorKind::TruncatedResponse,
                        "x",
                    ),
                    "translation.body",
                ),
            };
            let reason = error.retry_reason(Some(operation)).map(str::to_string);
            let want_reason = case
                .get("reason")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            assert_eq!(reason, want_reason, "{kind}");
            if let Some(category) = case.get("category").and_then(|v| v.as_str()) {
                assert_eq!(
                    errors::ProviderFailure::describe(&error).category,
                    category,
                    "{kind}"
                );
            }
        }
    }
}

#[test]
fn usage_ledger_matches_oracle() {
    let want = golden();
    let tracker = usage::UsageTracker::new();
    let attribution = usage::UsageAttribution {
        tier: "strong".to_string(),
        stage: "translation.body".to_string(),
        provider: "p1".to_string(),
        model: "m1".to_string(),
    };
    let mut labels = BTreeMap::new();
    labels.insert("p1".to_string(), "deepseek x".to_string());
    labels.insert("m1".to_string(), "deepseek / m".to_string());
    tracker.record(
        Some(usage::UsageSample {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            cache_hit_tokens: 20,
            cache_miss_tokens: 80,
            completion_ms: 1200,
        }),
        &attribution,
        &labels,
    );
    let attribution2 = usage::UsageAttribution {
        tier: "cheap".to_string(),
        stage: "glossary.curate".to_string(),
        provider: "p1".to_string(),
        model: "m2".to_string(),
    };
    tracker.record(
        Some(usage::UsageSample {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            ..Default::default()
        }),
        &attribution2,
        &BTreeMap::new(),
    );
    let summary = tracker.summary();
    assert_eq!(
        serde_json::Value::Object(summary.clone()),
        want["usage_summary"]
    );
    let merged = usage::merge_usage_summaries(&summary, &summary);
    assert_eq!(
        serde_json::Value::Object(merged.clone()),
        want["usage_merged"]
    );
    let delta = usage::usage_delta(&merged, &summary);
    assert_eq!(serde_json::Value::Object(delta), want["usage_delta"]);
}

#[test]
fn json_repairs_match_oracle() {
    let want = golden();
    for (name, text) in [
        ("strict", r#"{"a": 1}"#),
        ("missing_close", r#"{"a": 1"#),
        ("extra_close", r#"{"a": 1}}}"#),
        ("fenced", "```json\n{\"a\": 1}\n```"),
    ] {
        let result = json::parse_json_result(text).unwrap_or_else(|_| panic!("{name} parses"));
        let want_entry = &want["repairs"][name];
        assert_eq!(result.value, want_entry["value"], "{name} value");
        assert_eq!(
            result.repaired,
            want_entry["repaired"].as_bool().unwrap(),
            "{name} flag"
        );
        let kind = match result.repair_kind {
            json::RepairKind::None => "none",
            json::RepairKind::BoundaryOnly => "boundary_only",
            json::RepairKind::Structural => "structural",
        };
        assert_eq!(kind, want_entry["kind"].as_str().unwrap(), "{name} kind");
    }
    assert!(json::parse_json_result("The answer is 42.").is_err());
}

#[test]
fn fake_client_records_match_oracle() {
    use wenyi_core::llm::fake::FakeClient;
    let want = golden();
    let mut client = FakeClient::new(None, None).unwrap();
    client
        .complete(&messages(), "translation.body", false, None)
        .unwrap();
    client
        .complete(&messages(), "glossary.curate", true, Some(50))
        .unwrap();
    assert_eq!(client.calls.len(), 2);
    for (call, want_call) in client
        .calls
        .iter()
        .zip(want["fake_calls"].as_array().unwrap())
    {
        assert_eq!(call.operation, want_call["operation"].as_str().unwrap());
        assert_eq!(call.tier, want_call["tier"].as_str().unwrap());
        assert_eq!(call.json_mode, want_call["json_mode"].as_bool().unwrap());
        assert_eq!(call.max_tokens, want_call["max_tokens"].as_i64());
        assert_eq!(call.model, want_call["model"].as_str().unwrap());
        assert_eq!(call.provider, want_call["provider"].as_str().unwrap());
        assert_eq!(
            call.messages.len(),
            want_call["messages"].as_array().unwrap().len()
        );
    }
}
