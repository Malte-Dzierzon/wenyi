//! Fake-HTTP provider coverage: all 10 kinds against a stub server.
//!
//! A hand-rolled `TcpListener` stub (no server framework — `deps_guard`
//! forbids them) asserts per-kind request bodies/headers, then replays
//! scripted responses: success, retry-then-success, exhaustion, truncation,
//! empty, JSON fallback, usage normalization, cancellation, failover.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wenyi_core::config;
use wenyi_core::llm::client::RoutedClient;
use wenyi_core::llm::compat::Message;
use wenyi_core::llm::transport::Emit;

/// One scripted stub reply.
#[derive(Debug, Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

/// Observed stub request.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    headers: BTreeMap<String, String>,
    body: serde_json::Value,
}

/// Minimal blocking-IO HTTP stub (Chat Completions shaped).
struct Stub {
    addr: std::net::SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Stub {
    fn start(replies: Vec<Reply>) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let replies = Arc::new(Mutex::new(replies));
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let replies_clone = Arc::clone(&replies);
        let seen_clone = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                // Read headers + body (Content-Length framed).
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let body = loop {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break None,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if let Some(pos) = find_header_end(&buf) {
                                let head = String::from_utf8_lossy(&buf[..pos]).into_owned();
                                let content_len = head
                                    .lines()
                                    .find_map(|line| {
                                        let (k, v) = line.split_once(':')?;
                                        if k.trim().eq_ignore_ascii_case("content-length") {
                                            v.trim().parse::<usize>().ok()
                                        } else {
                                            None
                                        }
                                    })
                                    .unwrap_or(0);
                                let body_start = pos + 4;
                                while buf.len() < body_start + content_len {
                                    match stream.read(&mut chunk) {
                                        Ok(0) | Err(_) => break,
                                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                                    }
                                }
                                break Some((head, buf[body_start..].to_vec()));
                            }
                        }
                    }
                };
                let Some((head, body_bytes)) = body else {
                    continue;
                };
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or("");
                let mut parts = request_line.split_whitespace();
                let _method = parts.next().unwrap_or("");
                let path = parts.next().unwrap_or("").to_string();
                let mut headers = BTreeMap::new();
                for line in lines {
                    if let Some((k, v)) = line.split_once(':') {
                        headers.insert(k.trim().to_lowercase(), v.trim().to_string());
                    }
                }
                let body_json: serde_json::Value =
                    serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
                seen_clone.lock().unwrap().push(Seen {
                    path,
                    headers,
                    body: body_json,
                });
                let reply = {
                    let mut replies = replies_clone.lock().unwrap();
                    if replies.len() > 1 {
                        replies.remove(0)
                    } else {
                        replies[0].clone()
                    }
                };
                let status_text = match reply.status {
                    200 => "OK",
                    429 => "Too Many Requests",
                    500 => "Internal Server Error",
                    _ => "Error",
                };
                let mut response = format!(
                    "HTTP/1.1 {} {status_text}\r\ncontent-length: {}\r\n",
                    reply.status,
                    reply.body.len()
                );
                for (k, v) in &reply.headers {
                    response.push_str(&format!("{k}: {v}\r\n"));
                }
                response.push_str("connection: close\r\n\r\n");
                response.push_str(&reply.body);
                use std::io::Write;
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        let _ = &replies;
        Self { addr, seen }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn chat_body(text: &str, finish: &str) -> String {
    serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": text}, "finish_reason": finish}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
    })
    .to_string()
}

fn messages() -> Vec<Message> {
    vec![Message {
        role: "user".to_string(),
        content: "hi".to_string(),
    }]
}

fn client_for(yaml: &str) -> RoutedClient {
    let cfg = config::load(yaml).unwrap();
    let emit: Emit = Arc::new(|_, _| {});
    RoutedClient::new(cfg.llm, None, emit).unwrap()
}

fn compat_yaml(kind: &str, base_url: &str, extra: &str) -> String {
    format!(
        "llm:\n  providers:\n    default:\n      kind: {kind}\n      base_url: {base_url}\n      max_retries: 0\n{extra}  models:\n    m:\n      provider: default\n      model: test-model\n  tiers:\n    strong: m\n    cheap: m\n    fast: m\n"
    )
}

#[tokio::test]
async fn compat_request_body_matches_oracle_shape() {
    let stub = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: chat_body("hello", "stop"),
    }]);
    let client = client_for(&compat_yaml("openai-compatible", &stub.url(), ""));
    let text = client
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap();
    assert_eq!(text, "hello");
    let seen = stub.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/chat/completions");
    assert_eq!(seen[0].body["model"].as_str().unwrap(), "test-model");
    assert!(!seen[0].body["stream"].as_bool().unwrap());
    assert_eq!(seen[0].body["max_tokens"].as_i64().unwrap(), 8192);
    assert_eq!(
        seen[0].body["messages"][0]["content"].as_str().unwrap(),
        "hi"
    );
}

#[tokio::test]
async fn openai_uses_max_completion_tokens() {
    let stub = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: chat_body("hello", "stop"),
    }]);
    let client = client_for(&compat_yaml(
        "openai",
        &stub.url(),
        "      api_key_env: TEST_OPENAI_KEY\n",
    ));
    std::env::set_var("TEST_OPENAI_KEY", "test-key");
    let text = client
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap();
    assert_eq!(text, "hello");
    std::env::remove_var("TEST_OPENAI_KEY");
    let seen = stub.seen.lock().unwrap();
    assert_eq!(
        seen[0].body["max_completion_tokens"].as_i64().unwrap(),
        8192
    );
    assert!(seen[0].body.get("max_tokens").is_none());
    assert_eq!(seen[0].body["reasoning_effort"].as_str().unwrap(), "high");
    assert_eq!(
        seen[0].headers.get("authorization").unwrap(),
        "Bearer test-key"
    );
}

#[tokio::test]
async fn deepseek_thinking_body_and_disabled_variant() {
    let stub = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: chat_body("hello", "stop"),
    }]);
    let yaml = format!(
        "llm:\n  providers:\n    default:\n      kind: deepseek\n      base_url: {}\n      max_retries: 0\n      api_key_env: TEST_DS_KEY\n  models:\n    m:\n      provider: default\n      model: test-model\n  tiers:\n    strong: m\n    cheap: m\n    fast: m\n",
        stub.url()
    );
    std::env::set_var("TEST_DS_KEY", "k");
    let client = client_for(&yaml);
    client
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap();
    std::env::remove_var("TEST_DS_KEY");
    {
        let seen = stub.seen.lock().unwrap();
        assert_eq!(
            seen[0].body["extra_body"]["thinking"]["type"]
                .as_str()
                .unwrap(),
            "enabled"
        );
        assert_eq!(seen[0].body["reasoning_effort"].as_str().unwrap(), "high");
    }

    // Disabled thinking drops the effort key.
    let stub2 = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: chat_body("hello", "stop"),
    }]);
    let yaml2 = format!(
        "llm:\n  providers:\n    default:\n      kind: deepseek\n      base_url: {}\n      max_retries: 0\n      api_key_env: TEST_DS_KEY\n  models:\n    m:\n      provider: default\n      model: test-model\n      options:\n        thinking: false\n  tiers:\n    strong: m\n    cheap: m\n    fast: m\n",
        stub2.url()
    );
    std::env::set_var("TEST_DS_KEY", "k");
    let client2 = client_for(&yaml2);
    client2
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap();
    std::env::remove_var("TEST_DS_KEY");
    let seen2 = stub2.seen.lock().unwrap();
    assert_eq!(
        seen2[0].body["extra_body"]["thinking"]["type"]
            .as_str()
            .unwrap(),
        "disabled"
    );
    assert!(seen2[0].body.get("reasoning_effort").is_none());
}

#[tokio::test]
async fn openrouter_openai_reasoning_dialects() {
    for (kind, key) in [("openrouter", "TEST_OR_KEY"), ("openai", "TEST_OAI_KEY")] {
        let stub = Stub::start(vec![Reply {
            status: 200,
            headers: vec![],
            body: chat_body("hello", "stop"),
        }]);
        let client = client_for(&compat_yaml(
            kind,
            &stub.url(),
            &format!("      api_key_env: {key}\n"),
        ));
        std::env::set_var(key, "k");
        client
            .complete(&messages(), "translation.body", false, None)
            .await
            .unwrap();
        std::env::remove_var(key);
        let seen = stub.seen.lock().unwrap();
        if kind == "openrouter" {
            assert_eq!(
                seen[0].body["extra_body"]["reasoning"]["effort"]
                    .as_str()
                    .unwrap(),
                "high"
            );
        } else {
            assert_eq!(seen[0].body["reasoning_effort"].as_str().unwrap(), "high");
        }
    }
}

#[tokio::test]
async fn opencode_go_identity_headers_and_reasoning_cap() {
    let stub = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: chat_body("hello", "stop"),
    }]);
    let client = client_for(&compat_yaml(
        "opencode-go",
        &stub.url(),
        "      api_key_env: TEST_OC_KEY\n",
    ));
    std::env::set_var("TEST_OC_KEY", "k");
    client
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap();
    std::env::remove_var("TEST_OC_KEY");
    let seen = stub.seen.lock().unwrap();
    assert_eq!(seen[0].headers.get("user-agent").unwrap(), "wenyi");
    assert!(seen[0].headers.contains_key("x-opencode-session"));
    assert_eq!(
        seen[0].body["extra_body"]["reasoning_effort"]
            .as_str()
            .unwrap(),
        "low"
    );
}

#[tokio::test]
async fn compatible_reasoning_style_flows_from_connection() {
    let stub = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: chat_body("hello", "stop"),
    }]);
    let client = client_for(&compat_yaml(
        "openai-compatible",
        &stub.url(),
        "      reasoning_style: openrouter\n",
    ));
    client
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap();
    let seen = stub.seen.lock().unwrap();
    // thinking defaults false → explicit disable marker.
    assert!(!seen[0].body["extra_body"]["reasoning"]["enabled"]
        .as_bool()
        .unwrap());
}

#[tokio::test]
async fn retry_then_success_records_one_usage() {
    let stub = Stub::start(vec![
        Reply {
            status: 429,
            headers: vec![("retry-after".to_string(), "0".to_string())],
            body: "{}".to_string(),
        },
        Reply {
            status: 200,
            headers: vec![],
            body: chat_body("recovered", "stop"),
        },
    ]);
    let yaml = compat_yaml("openai-compatible", &stub.url(), "")
        .replace("max_retries: 0", "max_retries: 2");
    let client = client_for(&yaml);
    let text = client
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap();
    assert_eq!(text, "recovered");
    assert_eq!(stub.seen.lock().unwrap().len(), 2);
    let summary = client.summary();
    assert_eq!(summary["totals"]["calls"].as_i64().unwrap(), 1);
}

#[tokio::test]
async fn exhaustion_surfaces_safe_category() {
    let stub = Stub::start(vec![Reply {
        status: 500,
        headers: vec![],
        body: "{}".to_string(),
    }]);
    let yaml = compat_yaml("openai-compatible", &stub.url(), "")
        .replace("max_retries: 0", "max_retries: 1");
    let client = client_for(&yaml);
    let error = client
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("unavailable") || message.contains("provider"),
        "{message}"
    );
    assert_eq!(stub.seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn truncation_and_empty_are_safe_errors() {
    let stub = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: chat_body("cut…", "length"),
    }]);
    let client = client_for(&compat_yaml("openai-compatible", &stub.url(), ""));
    let error = client
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("truncated"), "{error}");

    let stub2 = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: chat_body("   ", "stop"),
    }]);
    let client2 = client_for(&compat_yaml("openai-compatible", &stub2.url(), ""));
    let error2 = client2
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap_err();
    assert!(error2.to_string().contains("no usable text"), "{error2}");
}

#[tokio::test]
async fn gemini_rest_dialect_round_trip() {
    let gemini_body = serde_json::json!({
        "candidates": [{
            "content": {"parts": [{"text": "bonjour"}]},
            "finishReason": "STOP",
        }],
        "usageMetadata": {
            "promptTokenCount": 7,
            "candidatesTokenCount": 3,
            "totalTokenCount": 10,
            "cachedContentTokenCount": 2,
        },
    })
    .to_string();
    let stub = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: gemini_body,
    }]);
    let yaml = format!(
        "llm:\n  providers:\n    default:\n      kind: gemini\n      base_url: {}\n      max_retries: 0\n      api_key_env: TEST_GEM_KEY\n  models:\n    m:\n      provider: default\n      model: gemini-x\n  tiers:\n    strong: m\n    cheap: m\n    fast: m\n",
        stub.url()
    );
    std::env::set_var("TEST_GEM_KEY", "gk");
    let client = client_for(&yaml);
    let text = client
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap();
    std::env::remove_var("TEST_GEM_KEY");
    assert_eq!(text, "bonjour");
    let seen = stub.seen.lock().unwrap();
    assert!(
        seen[0].path.contains(":generateContent"),
        "{}",
        seen[0].path
    );
    assert_eq!(seen[0].headers.get("x-goog-api-key").unwrap(), "gk");
    assert_eq!(
        seen[0].body["contents"][0]["role"].as_str().unwrap(),
        "user"
    );
    let summary = client.summary();
    assert_eq!(summary["totals"]["prompt_tokens"].as_i64().unwrap(), 7);
    assert_eq!(summary["totals"]["cache_hit_tokens"].as_i64().unwrap(), 2);
}

#[tokio::test]
async fn json_mode_sets_response_format() {
    let stub = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: chat_body(r#"{"a": 1}"#, "stop"),
    }]);
    let client = client_for(&compat_yaml("openai-compatible", &stub.url(), ""));
    let text = client
        .complete(&messages(), "translation.body", true, None)
        .await
        .unwrap();
    assert_eq!(text, r#"{"a": 1}"#);
    let seen = stub.seen.lock().unwrap();
    assert_eq!(
        seen[0].body["response_format"]["type"].as_str().unwrap(),
        "json_object"
    );
    assert!(seen[0].body["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("json"));
}

#[tokio::test]
async fn local_connection_failure_fails_fast_without_retry() {
    // Nothing listens here: loopback connection errors must not back off.
    let yaml = compat_yaml("ollama", "http://127.0.0.1:9/v1", "")
        .replace("max_retries: 0", "max_retries: 3");
    let client = client_for(&yaml);
    let started = std::time::Instant::now();
    let error = client
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "fail-fast took {:?}",
        started.elapsed()
    );
    assert!(
        error.to_string().contains("connect") || error.to_string().contains("provider"),
        "{error}"
    );
}

#[tokio::test]
async fn failover_tries_next_profile_on_retryable_error() {
    let bad = Stub::start(vec![Reply {
        status: 500,
        headers: vec![],
        body: "{}".to_string(),
    }]);
    let good = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: chat_body("via-fallback", "stop"),
    }]);
    let yaml = format!(
        "llm:\n  providers:\n    a:\n      kind: openai-compatible\n      base_url: {}\n      max_retries: 0\n    b:\n      kind: openai-compatible\n      base_url: {}\n      max_retries: 0\n  models:\n    primary:\n      provider: a\n      model: m\n    backup:\n      provider: b\n      model: m\n  tiers:\n    strong: primary\n    cheap: primary\n    fast: primary\n  routes:\n    translation.body:\n      model: primary\n      fallbacks: [backup]\n",
        bad.url(),
        good.url()
    );
    let client = client_for(&yaml);
    let text = client
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap();
    assert_eq!(text, "via-fallback");
    assert_eq!(bad.seen.lock().unwrap().len(), 1);
    assert_eq!(good.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn missing_credential_is_a_config_error_before_io() {
    let stub = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: chat_body("hello", "stop"),
    }]);
    std::env::remove_var("TEST_MISSING_KEY");
    let client = client_for(&compat_yaml(
        "openai",
        &stub.url(),
        "      api_key_env: TEST_MISSING_KEY\n",
    ));
    let error = client.validate_credentials(None).unwrap_err();
    assert!(error.to_string().contains("TEST_MISSING_KEY"), "{error}");
    assert!(stub.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn fake_kind_serves_without_http() {
    let cfg = config::load("llm:\n  preset: fake\n").unwrap();
    let emit: Emit = Arc::new(|_, _| {});
    let client = RoutedClient::new(cfg.llm, None, emit).unwrap();
    let text = client
        .complete(&messages(), "translation.body", true, None)
        .await
        .unwrap();
    assert_eq!(text, "[]");
}

#[tokio::test]
async fn compat_list_models_pages_like_sdk() {
    let stub = Stub::start(vec![
        Reply {
            status: 200,
            headers: vec![],
            body: serde_json::json!({
                "data": [{"id": "a"}, {"id": ""}],
                "next_page": "cursor-1",
            })
            .to_string(),
        },
        Reply {
            status: 200,
            headers: vec![],
            body: serde_json::json!({"data": [{"id": "b"}]}).to_string(),
        },
    ]);
    let client = client_for(&compat_yaml("openai-compatible", &stub.url(), ""));
    let names = client.list_models("default").await.unwrap();
    assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    let seen = stub.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].path, "/models");
    assert_eq!(seen[1].path, "/models?after=cursor-1");
}

#[tokio::test]
async fn gemini_list_models_filters_actions_and_prefix() {
    let stub = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: serde_json::json!({
            "models": [
                {"name": "models/gemini-x", "supportedActions": ["generateContent"]},
                {"name": "models/embed", "supportedActions": ["embedContent"]},
                {"name": "models/legacy"},
            ],
        })
        .to_string(),
    }]);
    let yaml = format!(
        "llm:\n  providers:\n    default:\n      kind: gemini\n      base_url: {}\n      max_retries: 0\n      api_key_env: TEST_GEM_LIST\n  models:\n    m:\n      provider: default\n      model: gemini-x\n  tiers:\n    strong: m\n    cheap: m\n    fast: m\n",
        stub.url()
    );
    std::env::set_var("TEST_GEM_LIST", "gk");
    let client = client_for(&yaml);
    let names = client.list_models("default").await.unwrap();
    std::env::remove_var("TEST_GEM_LIST");
    assert_eq!(names, vec!["gemini-x".to_string(), "legacy".to_string()]);
    let seen = stub.seen.lock().unwrap();
    assert_eq!(seen[0].path, "/v1beta/models");
    assert_eq!(seen[0].headers.get("x-goog-api-key").unwrap(), "gk");
}

#[tokio::test]
async fn thin_local_kinds_share_compat_wire() {
    // ollama/vllm/llamacpp/orcarouter are endpoint/credential presets over
    // the shared compatible core; one body assertion each locks the wiring.
    for (kind, extra) in [
        ("ollama", ""),
        ("vllm", ""),
        ("llamacpp", ""),
        ("orcarouter", "      api_key_env: TEST_ORCA_KEY\n"),
    ] {
        let stub = Stub::start(vec![Reply {
            status: 200,
            headers: vec![],
            body: chat_body("ok", "stop"),
        }]);
        let client = client_for(&compat_yaml(kind, &stub.url(), extra));
        if kind == "orcarouter" {
            std::env::set_var("TEST_ORCA_KEY", "k");
        }
        let text = client
            .complete(&messages(), "translation.body", false, None)
            .await
            .unwrap();
        if kind == "orcarouter" {
            std::env::remove_var("TEST_ORCA_KEY");
        }
        assert_eq!(text, "ok", "{kind}");
        let seen = stub.seen.lock().unwrap();
        assert_eq!(seen[0].path, "/chat/completions", "{kind}");
        assert_eq!(
            seen[0].body["model"].as_str().unwrap(),
            "test-model",
            "{kind}"
        );
        if kind == "orcarouter" {
            assert_eq!(seen[0].headers.get("authorization").unwrap(), "Bearer k");
        }
    }
}

#[tokio::test]
async fn attempts_record_usage_once_per_call() {
    let stub = Stub::start(vec![Reply {
        status: 200,
        headers: vec![],
        body: chat_body("hello", "stop"),
    }]);
    let client = client_for(&compat_yaml("openai-compatible", &stub.url(), ""));
    client
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap();
    client
        .complete(&messages(), "translation.body", false, None)
        .await
        .unwrap();
    let summary = client.summary();
    assert_eq!(summary["totals"]["calls"].as_i64().unwrap(), 2);
    assert_eq!(summary["totals"]["prompt_tokens"].as_i64().unwrap(), 20);
    assert_eq!(summary["by_tier"]["strong"]["calls"].as_i64().unwrap(), 2);
}
