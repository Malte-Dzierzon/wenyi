//! Review loop: immediate-final verification, dismissal rules, fallback.

use std::collections::BTreeMap;
use wenyi_core::config;
use wenyi_core::glossary::GlossaryTerm;
use wenyi_core::i18n::policy;
use wenyi_core::llm::compat::Message;
use wenyi_core::llm::fake::{FakeClient, FakeHandler};
use wenyi_core::pipeline_agents::ModelClient;
use wenyi_core::review_evidence::{BookEvidenceIndex, EvidenceChapter};
use wenyi_core::review_loop::{MemoryTrace, ReviewAgentLoop};

fn cfg() -> config::Config {
    config::load("language: {source: en, target: zh}\nllm:\n  preset: fake\n  models:\n    default_strong: {provider: default, model: p}\n    default_cheap: {provider: default, model: f}\n").unwrap()
}

fn chapters() -> Vec<EvidenceChapter> {
    vec![EvidenceChapter {
        index: 0,
        title: "C".to_string(),
        digest: String::new(),
        segments: vec![
            (
                0,
                0,
                "The old Wolf prowls.".to_string(),
                "老狼潜行。".to_string(),
            ),
            (1, 1, "Moon rises.".to_string(), "月升。".to_string()),
        ],
    }]
}

fn candidate(index: i64) -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({"index": index, "type": "mistranslation", "detail": "d", "suggestion": "s"})
        .as_object()
        .unwrap()
        .clone()
}

fn plan(cfg: &config::Config) -> policy::PolicyPlan {
    let groups =
        policy::task_groups_full("review", true, true, false, true, true, true, true).unwrap();
    let refs: Vec<&str> = groups.iter().map(|s| s as &str).collect();
    policy::resolve_policy("en", "zh", "review", &refs, &cfg.honorific_strategy).unwrap()
}

#[test]
fn immediate_final_confirms_and_dismisses() {
    let cfg = cfg();
    let mut evidence = BookEvidenceIndex::new(
        &chapters(),
        vec![GlossaryTerm::new("Wolf", "狼")],
        serde_json::Map::new(),
        "en",
        &BTreeMap::new(),
    );
    let handler: FakeHandler = Box::new(|_: &[Message], _: &str, _: bool| {
        serde_json::json!({
            "action": "final",
            "decisions": [
                {"candidate_id": "r1-ch0-base0-candidate0", "verdict": "confirmed",
                 "detail": "Wrong name", "suggestion": "Fix it",
                 "consistency": {}, "evidence_refs": []},
                {"candidate_id": "r1-ch0-base0-candidate1", "verdict": "dismissed",
                 "reason": "Style is fine", "evidence_refs": []},
            ],
            "new_issues": [],
            "complete": true,
        })
        .to_string()
    });
    let mut fake = FakeClient::new(Some(handler), None).unwrap();
    let plan = plan(&cfg);
    let mut agent =
        ReviewAgentLoop::new("en", "zh", plan, &cfg, &mut evidence, "review.verify").unwrap();
    let mut trace = MemoryTrace::default();
    let sources = vec![
        "The old Wolf prowls.".to_string(),
        "Moon rises.".to_string(),
    ];
    let targets = vec!["老狼潜行。".to_string(), "月升。".to_string()];
    let outcome = agent.review_chunk(
        &mut fake,
        &mut trace,
        0,
        0,
        &sources,
        &targets,
        &[candidate(0), candidate(1)],
        Some(1),
    );
    assert!(
        outcome.fallback_reason.is_empty(),
        "{}",
        outcome.fallback_reason
    );
    assert_eq!(outcome.issues.len(), 1);
    assert_eq!(outcome.issues[0]["origin"], "initial");
    assert_eq!(outcome.issues[0]["detail"], "Wrong name");
    assert!(outcome.issues[0]["evidence_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "ch0:text0:seg0"));
    assert_eq!(outcome.dismissed.len(), 1);
    assert_eq!(fake.call_count(), 1);
}

#[test]
fn incomplete_decisions_fall_back_to_initial() {
    let cfg = cfg();
    let mut evidence = BookEvidenceIndex::new(
        &chapters(),
        vec![],
        serde_json::Map::new(),
        "en",
        &BTreeMap::new(),
    );
    let handler: FakeHandler = Box::new(|_: &[Message], _: &str, _: bool| {
        // Missing the second candidate decision: protocol violation.
        serde_json::json!({
            "action": "final",
            "decisions": [
                {"candidate_id": "ch0-base0-candidate0", "verdict": "confirmed",
                 "detail": "x", "suggestion": "y", "consistency": {}, "evidence_refs": []},
            ],
            "new_issues": [],
            "complete": true,
        })
        .to_string()
    });
    let mut fake = FakeClient::new(Some(handler), None).unwrap();
    let plan = plan(&cfg);
    let mut agent =
        ReviewAgentLoop::new("en", "zh", plan, &cfg, &mut evidence, "review.verify").unwrap();
    let mut trace = MemoryTrace::default();
    let sources = vec!["a".to_string(), "b".to_string()];
    let targets = vec!["A".to_string(), "B".to_string()];
    let outcome = agent.review_chunk(
        &mut fake,
        &mut trace,
        0,
        0,
        &sources,
        &targets,
        &[candidate(0), candidate(1)],
        None,
    );
    assert!(!outcome.fallback_reason.is_empty());
    assert_eq!(outcome.issues.len(), 2);
    assert_eq!(outcome.issues[0]["agent_fallback"], true);
    assert!(trace
        .events
        .iter()
        .any(|e| e["event"] == "review_agent_fallback"));
}

#[test]
fn evidence_round_serves_tool_results_then_final() {
    let cfg = cfg();
    let mut evidence = BookEvidenceIndex::new(
        &chapters(),
        vec![GlossaryTerm::new("Wolf", "狼")],
        serde_json::Map::new(),
        "en",
        &BTreeMap::new(),
    );
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let handler: FakeHandler = Box::new(move |_: &[Message], _: &str, _: bool| {
        let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == 0 {
            serde_json::json!({
                "action": "request_evidence",
                "requests": [
                    {"request_id": "q1", "tool": "glossary_term", "arguments": {"term": "Wolf"}},
                ],
                "complete": false,
            })
            .to_string()
        } else {
            serde_json::json!({
                "action": "final",
                "decisions": [
                    {"candidate_id": "ch0-base0-candidate0", "verdict": "dismissed",
                     "reason": "glossary confirms", "evidence_refs": []},
                ],
                "new_issues": [],
                "complete": true,
            })
            .to_string()
        }
    });
    let mut fake = FakeClient::new(Some(handler), None).unwrap();
    let plan = plan(&cfg);
    let mut agent =
        ReviewAgentLoop::new("en", "zh", plan, &cfg, &mut evidence, "review.verify").unwrap();
    let mut trace = MemoryTrace::default();
    let sources = vec!["The old Wolf prowls.".to_string()];
    let targets = vec!["老狼潜行。".to_string()];
    let outcome = agent.review_chunk(
        &mut fake,
        &mut trace,
        0,
        0,
        &sources,
        &targets,
        &[candidate(0)],
        None,
    );
    assert!(
        outcome.fallback_reason.is_empty(),
        "{}",
        outcome.fallback_reason
    );
    assert_eq!(outcome.dismissed.len(), 1);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(trace
        .events
        .iter()
        .any(|e| e["event"] == "review_evidence_supplied"));
}
