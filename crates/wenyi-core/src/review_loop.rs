//! Bounded review action loop + verify agent. Oracle:
//! `agents/review_actions.py` + `agents/review_loop.py`.
//!
//! `ReviewActionLoop::run` drives request-evidence/final turns with trace
//! persistence + resume, request dedup, and a 128 KiB evidence batch cap.
//! Failures fall back to the initial review by contract (never raise).
//! `ReviewAgentLoop::review_chunk` verifies one leaf block.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::config::Config;
use crate::llm::compat::Message;
use crate::llm::json::parse_json_result;
use crate::pipeline_agents::{numbered_pairs_with_refs, Agent, ModelClient};
use crate::review_evidence::BookEvidenceIndex;
use crate::review_models::{clean_text, review_candidate_id, CONSISTENCY_KINDS};
use crate::{Error, Result};

/// Agent loop protocol violation (oracle `ReviewLoopProtocolError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewLoopProtocolError {
    /// Stable reason code.
    pub reason: String,
}

impl std::fmt::Display for ReviewLoopProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Review loop protocol error: {}", self.reason)
    }
}

impl std::error::Error for ReviewLoopProtocolError {}

/// Evidence batch byte cap (oracle `run`).
pub const EVIDENCE_BATCH_LIMIT: usize = 128_000;

/// Verify final output cites only obtained evidence (oracle
/// `validate_evidence_refs`).
pub fn validate_evidence_refs(
    value: &serde_json::Value,
    allowed_refs: &BTreeSet<String>,
) -> Result<Vec<String>, ReviewLoopProtocolError> {
    let fail = |reason: &str| ReviewLoopProtocolError {
        reason: reason.to_string(),
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let items = value
        .as_array()
        .ok_or_else(|| fail("invalid_evidence_refs"))?;
    if items.iter().any(|item| !item.is_string()) {
        return Err(fail("invalid_evidence_refs"));
    }
    let mut refs = Vec::new();
    for item in items {
        let reference = item.as_str().unwrap().to_string();
        if !refs.contains(&reference) {
            refs.push(reference);
        }
    }
    if refs
        .iter()
        .any(|reference| !allowed_refs.contains(reference))
    {
        return Err(fail("unknown_evidence_ref"));
    }
    Ok(refs)
}

/// Agent snapshot + diagnostics port (oracle `ReviewTrace`).
pub trait ReviewTrace {
    /// Load a persisted agent trace.
    fn load(&self, agent_id: &str) -> Option<serde_json::Map<String, serde_json::Value>>;
    /// Save an agent trace snapshot.
    fn save(&mut self, agent_id: &str, snapshot: &serde_json::Map<String, serde_json::Value>);
    /// Append a trace event.
    fn log_event(&mut self, event: &str, data: serde_json::Map<String, serde_json::Value>);
}

/// In-memory trace (tests + single-run sessions).
#[derive(Debug, Default)]
pub struct MemoryTrace {
    /// Snapshots by agent id.
    pub snapshots: BTreeMap<String, serde_json::Map<String, serde_json::Value>>,
    /// Logged events.
    pub events: Vec<serde_json::Value>,
}

impl ReviewTrace for MemoryTrace {
    fn load(&self, agent_id: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
        self.snapshots.get(agent_id).cloned()
    }

    fn save(&mut self, agent_id: &str, snapshot: &serde_json::Map<String, serde_json::Value>) {
        self.snapshots
            .insert(agent_id.to_string(), snapshot.clone());
    }

    fn log_event(&mut self, event: &str, data: serde_json::Map<String, serde_json::Value>) {
        let mut entry = serde_json::Map::new();
        entry.insert("event".to_string(), json!(event));
        for (key, value) in data {
            entry.insert(key, value);
        }
        self.events.push(serde_json::Value::Object(entry));
    }
}

/// Final-turn validator: parsed turn + allowlist -> result.
pub type ValidateFinal<'a> = dyn Fn(
        &serde_json::Map<String, serde_json::Value>,
        &BTreeSet<String>,
    ) -> Result<serde_json::Value, ReviewLoopProtocolError>
    + 'a;

/// Replayed conversation state (oracle `ConversationState`).
pub struct ConversationState {
    /// Replayed messages.
    pub messages: Vec<Message>,
    /// Citable references.
    pub allowed_refs: BTreeSet<String>,
    /// Max evidence rounds.
    pub max_rounds: i64,
    /// Completed evidence rounds.
    pub evidence_rounds: i64,
    /// Claimed request ids.
    pub seen_request_ids: BTreeSet<String>,
    /// Claimed (tool, canonical-arguments) signatures.
    pub seen_requests: BTreeSet<(String, String)>,
}

impl ConversationState {
    /// Restore completed rounds; return the first unfinished turn.
    pub fn replay(&mut self, resume_turns: &[serde_json::Map<String, serde_json::Value>]) -> usize {
        for cached in resume_turns {
            let results = cached.get("evidence_results").and_then(|v| v.as_array());
            let raw = cached.get("raw_response").and_then(|v| v.as_str());
            let (Some(results), Some(raw)) = (results, raw) else {
                continue;
            };
            self.messages.push(Message {
                role: "assistant".to_string(),
                content: raw.to_string(),
            });
            let mut evidence_message = format!(
                "[Evidence tool results (JSON)]\n{}",
                serde_json::Value::Array(results.clone())
            );
            self.evidence_rounds += 1;
            if self.evidence_rounds >= self.max_rounds {
                evidence_message += "\nEvidence rounds are exhausted. The next response must use action=final; do not request more evidence.";
            }
            self.messages.push(Message {
                role: "user".to_string(),
                content: evidence_message,
            });
            self.allowed_refs
                .extend(BookEvidenceIndex::evidence_refs(&serde_json::Value::Array(
                    results.clone(),
                )));
            if let Some(parsed) = cached.get("parsed").and_then(|v| v.as_object()) {
                if let Some(requests) = parsed.get("requests").and_then(|v| v.as_array()) {
                    for request in requests {
                        let Some(request) = request.as_object() else {
                            continue;
                        };
                        let request_id =
                            clean_text(request.get("request_id").unwrap_or(&json!(null)));
                        if !request_id.is_empty() {
                            self.seen_request_ids.insert(request_id);
                        }
                        let tool = clean_text(request.get("tool").unwrap_or(&json!(null)));
                        if let Some(arguments) =
                            request.get("arguments").and_then(|v| v.as_object())
                        {
                            if !tool.is_empty() {
                                self.seen_requests.insert((
                                    tool,
                                    serde_json::Value::Object(arguments.clone()).to_string(),
                                ));
                            }
                        }
                    }
                }
            }
        }
        let mut start_turn = resume_turns.len();
        if resume_turns
            .last()
            .is_some_and(|turn| turn.contains_key("evidence_results"))
        {
            start_turn += 1;
        }
        start_turn
    }

    /// Validate a request group before reserving ids + signatures.
    pub fn claim_requests(
        &mut self,
        data: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Vec<serde_json::Map<String, serde_json::Value>>, ReviewLoopProtocolError> {
        let fail = |reason: &str| ReviewLoopProtocolError {
            reason: reason.to_string(),
        };
        if data.get("complete") != Some(&json!(false)) {
            return Err(fail("evidence_action_marked_complete"));
        }
        if self.evidence_rounds >= self.max_rounds {
            return Err(fail("evidence_round_limit"));
        }
        let requests = data
            .get("requests")
            .and_then(|v| v.as_array())
            .ok_or_else(|| fail("invalid_evidence_requests"))?;
        if requests.is_empty() || requests.len() > 4 {
            return Err(fail("invalid_evidence_requests"));
        }
        let mut current_ids = BTreeSet::new();
        let mut current_requests = BTreeSet::new();
        let mut out = Vec::new();
        for request in requests {
            let request = request
                .as_object()
                .ok_or_else(|| fail("evidence_request_not_object"))?;
            let request_id = clean_text(request.get("request_id").unwrap_or(&json!(null)));
            if request_id.is_empty()
                || self.seen_request_ids.contains(&request_id)
                || current_ids.contains(&request_id)
            {
                return Err(fail("duplicate_evidence_request_id"));
            }
            let tool = clean_text(request.get("tool").unwrap_or(&json!(null)));
            let arguments = request
                .get("arguments")
                .and_then(|v| v.as_object())
                .ok_or_else(|| fail("invalid_evidence_request"))?;
            if tool.is_empty() {
                return Err(fail("invalid_evidence_request"));
            }
            let signature = (
                tool,
                serde_json::Value::Object(arguments.clone()).to_string(),
            );
            if self.seen_requests.contains(&signature) || current_requests.contains(&signature) {
                return Err(fail("duplicate_evidence_request"));
            }
            current_ids.insert(request_id);
            current_requests.insert(signature);
            out.push(request.clone());
        }
        self.seen_request_ids.extend(current_ids);
        self.seen_requests.extend(current_requests);
        Ok(out)
    }
}

/// Request-evidence/final loop (oracle `ReviewActionLoop`).
pub struct ReviewActionLoop<'a> {
    /// Max evidence rounds.
    pub max_rounds: i64,
    /// Inference fingerprint for trace invalidation.
    pub inference: serde_json::Value,
    /// Evidence index.
    pub evidence: &'a mut BookEvidenceIndex,
}

impl<'a> ReviewActionLoop<'a> {
    /// Bind evidence + config limits.
    pub fn new(evidence: &'a mut BookEvidenceIndex, config: &Config, stage: &str) -> Result<Self> {
        let snapshot = crate::llm::routing::inference_snapshot(&config.llm, &[stage.to_string()])?;
        Ok(Self {
            max_rounds: config.pipeline.review_agent_max_evidence_rounds,
            inference: serde_json::Value::Object(snapshot.into_iter().collect()),
            evidence,
        })
    }

    /// Run up to N evidence rounds + final; failure reason instead of raise.
    /// Final-turn validator shared by verify and arbiter loops.
    #[allow(clippy::too_many_arguments)] // Mirrors the oracle run shape.
    pub fn run(
        &mut self,
        client: &mut dyn ModelClient,
        trace: &mut dyn ReviewTrace,
        agent_id: &str,
        system: &str,
        user: &str,
        stage: &str,
        allowed_refs: BTreeSet<String>,
        validate_final: &ValidateFinal<'_>,
    ) -> (Option<serde_json::Value>, String) {
        let mut messages = vec![
            Message {
                role: "system".to_string(),
                content: system.to_string(),
            },
            Message {
                role: "user".to_string(),
                content: user.to_string(),
            },
        ];
        if self.max_rounds == 0 {
            messages[1].content +=
                "\nEvidence requests are disabled for this call; return final immediately.";
        }
        let mut snapshot = serde_json::Map::new();
        snapshot.insert("agent_id".to_string(), json!(agent_id));
        snapshot.insert("stage".to_string(), json!(stage));
        snapshot.insert("status".to_string(), json!("running"));
        snapshot.insert("turns".to_string(), json!([]));
        snapshot.insert("inference".to_string(), self.inference.clone());
        let mut existing = trace.load(agent_id);
        if existing
            .as_ref()
            .is_some_and(|trace| trace.get("inference") != Some(&self.inference))
        {
            let mut data = serde_json::Map::new();
            data.insert("agent_id".to_string(), json!(agent_id));
            data.insert("operation".to_string(), json!(stage));
            data.insert("reason".to_string(), json!("request_changed"));
            trace.log_event("review_agent_cache_invalidated", data);
            existing = None;
        }
        let mut resume_turns: Vec<serde_json::Map<String, serde_json::Value>> = Vec::new();
        if let Some(existing) = existing {
            match existing.get("status").and_then(|v| v.as_str()) {
                Some("finished") if existing.get("result").is_some_and(|v| v.is_object()) => {
                    let mut data = serde_json::Map::new();
                    data.insert("agent_id".to_string(), json!(agent_id));
                    data.insert("stage".to_string(), json!(stage));
                    data.insert(
                        "turns".to_string(),
                        json!(existing
                            .get("turns")
                            .and_then(|v| v.as_array())
                            .map_or(0, |t| t.len() as i64)),
                    );
                    data.insert("resumed".to_string(), json!(true));
                    trace.log_event("review_agent_finished", data);
                    return (existing.get("result").cloned(), String::new());
                }
                Some("fallback") => {
                    let reason = existing
                        .get("fallback_reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let mut data = serde_json::Map::new();
                    data.insert("agent_id".to_string(), json!(agent_id));
                    data.insert("stage".to_string(), json!(stage));
                    data.insert("reason".to_string(), json!(reason.clone()));
                    data.insert("resumed".to_string(), json!(true));
                    trace.log_event("review_agent_fallback", data);
                    return (None, reason);
                }
                Some("running") => {
                    resume_turns = existing
                        .get("turns")
                        .and_then(|v| v.as_array())
                        .map(|turns| {
                            turns
                                .iter()
                                .filter_map(|t| t.as_object().cloned())
                                .collect()
                        })
                        .unwrap_or_default();
                }
                _ => {}
            }
        }
        snapshot.insert(
            "turns".to_string(),
            serde_json::Value::Array(
                resume_turns
                    .iter()
                    .map(|t| serde_json::Value::Object(t.clone()))
                    .collect(),
            ),
        );
        let mut conversation = ConversationState {
            messages,
            allowed_refs,
            max_rounds: self.max_rounds,
            evidence_rounds: 0,
            seen_request_ids: BTreeSet::new(),
            seen_requests: BTreeSet::new(),
        };
        let mut start_turn = 1usize;
        if !resume_turns.is_empty() {
            if let Some(first) = resume_turns[0].get("messages").and_then(|v| v.as_array()) {
                conversation.messages = first
                    .iter()
                    .filter_map(|message| {
                        Some(Message {
                            role: message.get("role")?.as_str()?.to_string(),
                            content: message.get("content")?.as_str()?.to_string(),
                        })
                    })
                    .collect();
            }
            let mut data = serde_json::Map::new();
            data.insert("agent_id".to_string(), json!(agent_id));
            data.insert("stage".to_string(), json!(stage));
            data.insert("turns".to_string(), json!(resume_turns.len() as i64));
            trace.log_event("review_agent_resumed", data);
            start_turn = conversation.replay(&resume_turns);
        }
        trace.save(agent_id, &snapshot);
        let cached_by_turn: BTreeMap<i64, serde_json::Map<String, serde_json::Value>> =
            resume_turns
                .into_iter()
                .filter_map(|turn| turn.get("turn").and_then(|v| v.as_i64()).map(|n| (n, turn)))
                .collect();
        let outcome: Result<(Option<serde_json::Value>, String), ReviewLoopProtocolError> =
            (|| {
                let last = start_turn
                    .max((self.max_rounds + 1) as usize)
                    .max(start_turn);
                for turn_number in start_turn..=last.max(start_turn) {
                    let _ = last;
                    let sent: Vec<serde_json::Value> = conversation
                        .messages
                        .iter()
                        .map(|message| json!({"role": message.role, "content": message.content}))
                        .collect();
                    let mut turns = snapshot
                        .get("turns")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let turn_index = turns.iter().position(|turn| {
                        turn.get("turn").and_then(|v| v.as_i64()) == Some(turn_number as i64)
                    });
                    let cached_turn = cached_by_turn.get(&(turn_number as i64)).cloned();
                    if let Some(index) = turn_index {
                        turns[index].as_object_mut().unwrap().insert(
                            "messages".to_string(),
                            serde_json::Value::Array(sent.clone()),
                        );
                    } else {
                        let mut turn = serde_json::Map::new();
                        turn.insert("turn".to_string(), json!(turn_number as i64));
                        turn.insert("messages".to_string(), serde_json::Value::Array(sent));
                        turn.insert("status".to_string(), json!("requesting"));
                        turns.push(serde_json::Value::Object(turn));
                    }
                    snapshot.insert("turns".to_string(), serde_json::Value::Array(turns));
                    trace.save(agent_id, &snapshot);
                    let raw = if cached_turn
                        .as_ref()
                        .is_some_and(|turn| turn.get("raw_response").is_some_and(|v| v.is_string()))
                    {
                        cached_turn.as_ref().unwrap()["raw_response"]
                            .as_str()
                            .unwrap()
                            .to_string()
                    } else {
                        match client.complete(&conversation.messages, stage, true, None) {
                            Ok(raw) => raw,
                            Err(error) => {
                                Self::mark_turn(
                                    &mut snapshot,
                                    turn_number as i64,
                                    "failed",
                                    Some((
                                        "error",
                                        json!({"type": error_kind(&error), "message": error.to_string()}),
                                    )),
                                );
                                trace.save(agent_id, &snapshot);
                                return Err(ReviewLoopProtocolError {
                                    reason: format!("{}: {error}", error_kind(&error)),
                                });
                            }
                        }
                    };
                    Self::mark_turn(
                        &mut snapshot,
                        turn_number as i64,
                        "responded",
                        Some(("raw_response", json!(raw.clone()))),
                    );
                    trace.save(agent_id, &snapshot);
                    let data = if cached_turn.as_ref().is_some_and(|turn| {
                        turn.get("raw_response").is_some_and(|v| v.is_string())
                            && turn.get("parsed").is_some_and(|v| v.is_object())
                    }) {
                        let cached = cached_turn.as_ref().unwrap();
                        let parsed =
                            parse_json_result(&raw).map_err(|_| ReviewLoopProtocolError {
                                reason: "malformed_json".to_string(),
                            })?;
                        let data = cached["parsed"].as_object().unwrap().clone();
                        // Raw and parsed must come from the same cached response.
                        let reparsed = serde_json::Value::Object(data.clone()).to_string();
                        let fresh = serde_json::Value::Object(
                            parsed.value.as_object().cloned().unwrap_or_default(),
                        )
                        .to_string();
                        if fresh != reparsed
                            && serde_json::to_string(&parsed.value).unwrap_or_default()
                                != serde_json::to_string(&serde_json::Value::Object(data.clone()))
                                    .unwrap_or_default()
                        {
                            return Err(ReviewLoopProtocolError {
                                reason: "unsafe_json_repair".to_string(),
                            });
                        }
                        Self::mark_turn(
                            &mut snapshot,
                            turn_number as i64,
                            "responded",
                            Some(("parsed", serde_json::Value::Object(data.clone()))),
                        );
                        Self::mark_turn(
                            &mut snapshot,
                            turn_number as i64,
                            "responded",
                            Some(("json_repaired", json!(parsed.repaired))),
                        );
                        data
                    } else {
                        let parsed =
                            parse_json_result(&raw).map_err(|_| ReviewLoopProtocolError {
                                reason: "malformed_json".to_string(),
                            })?;
                        let data = parsed.value.as_object().cloned().ok_or_else(|| {
                            ReviewLoopProtocolError {
                                reason: "response_not_object".to_string(),
                            }
                        })?;
                        // NLL: check repair safety before moving `parsed`.
                        let safe = parsed.safe_for_complete_payload();
                        let repaired = parsed.repaired;
                        Self::mark_turn(
                            &mut snapshot,
                            turn_number as i64,
                            "responded",
                            Some(("parsed", serde_json::Value::Object(data.clone()))),
                        );
                        Self::mark_turn(
                            &mut snapshot,
                            turn_number as i64,
                            "responded",
                            Some(("json_repaired", json!(repaired))),
                        );
                        if !safe {
                            return Err(ReviewLoopProtocolError {
                                reason: "unsafe_json_repair".to_string(),
                            });
                        }
                        data
                    };
                    trace.save(agent_id, &snapshot);
                    let action = data
                        .get("action")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    if action == "final" {
                        if data.get("complete") != Some(&json!(true)) {
                            return Err(ReviewLoopProtocolError {
                                reason: "final_not_complete".to_string(),
                            });
                        }
                        let result = validate_final(&data, &conversation.allowed_refs)?;
                        snapshot.insert("status".to_string(), json!("finished"));
                        snapshot.insert("result".to_string(), result.clone());
                        trace.save(agent_id, &snapshot);
                        let mut event = serde_json::Map::new();
                        event.insert("agent_id".to_string(), json!(agent_id));
                        event.insert("stage".to_string(), json!(stage));
                        event.insert("turns".to_string(), json!(turn_number as i64));
                        event.insert(
                            "evidence_rounds".to_string(),
                            json!(conversation.evidence_rounds),
                        );
                        trace.log_event("review_agent_finished", event);
                        return Ok((Some(result), String::new()));
                    }
                    if action != "request_evidence" {
                        return Err(ReviewLoopProtocolError {
                            reason: "unknown_action".to_string(),
                        });
                    }
                    let requests = conversation.claim_requests(&data)?;
                    let mut results = Vec::new();
                    let mut batch_size = 2usize;
                    for request in &requests {
                        let mut result = self.evidence.execute(request);
                        let mut encoded_size =
                            serde_json::Value::Object(result.clone()).to_string().len();
                        if batch_size + encoded_size > EVIDENCE_BATCH_LIMIT {
                            result = serde_json::Map::new();
                            result.insert(
                                "request_id".to_string(),
                                request.get("request_id").cloned().unwrap_or(json!("")),
                            );
                            result.insert(
                                "tool".to_string(),
                                request.get("tool").cloned().unwrap_or(json!("")),
                            );
                            result.insert("ok".to_string(), json!(false));
                            result.insert("error".to_string(), json!("evidence_batch_too_large"));
                            result.insert(
                            "hint".to_string(),
                            json!("Reduce the requests per round or the context range of each request."),
                        );
                            encoded_size =
                                serde_json::Value::Object(result.clone()).to_string().len();
                        }
                        results.push(serde_json::Value::Object(result));
                        batch_size += encoded_size + 1;
                    }
                    conversation.evidence_rounds += 1;
                    conversation
                        .allowed_refs
                        .extend(BookEvidenceIndex::evidence_refs(&serde_json::Value::Array(
                            results.clone(),
                        )));
                    Self::mark_turn(
                        &mut snapshot,
                        turn_number as i64,
                        "responded",
                        Some((
                            "evidence_results",
                            serde_json::Value::Array(results.clone()),
                        )),
                    );
                    trace.save(agent_id, &snapshot);
                    let mut event = serde_json::Map::new();
                    event.insert("agent_id".to_string(), json!(agent_id));
                    event.insert("stage".to_string(), json!(stage));
                    event.insert("round".to_string(), json!(conversation.evidence_rounds));
                    event.insert(
                        "requests".to_string(),
                        json!(requests
                            .iter()
                            .map(|request| {
                                json!({
                                    "request_id": request.get("request_id"),
                                    "tool": request.get("tool"),
                                    "arguments": request.get("arguments"),
                                })
                            })
                            .collect::<Vec<_>>()),
                    );
                    event.insert(
                        "refs".to_string(),
                        json!(
                            BookEvidenceIndex::evidence_refs(&serde_json::Value::Array(results))
                                .into_iter()
                                .collect::<Vec<_>>()
                        ),
                    );
                    trace.log_event("review_evidence_supplied", event);
                    conversation.messages.push(Message {
                        role: "assistant".to_string(),
                        content: raw,
                    });
                    let mut evidence_message = format!(
                        "[Evidence tool results (JSON)]\n{}",
                        serde_json::Value::Array(
                            snapshot["turns"].as_array().unwrap().last().unwrap()
                                ["evidence_results"]
                                .as_array()
                                .unwrap()
                                .clone()
                        )
                    );
                    if conversation.evidence_rounds >= self.max_rounds {
                        evidence_message += "\nEvidence rounds are exhausted. The next response must use action=final; do not request more evidence.";
                    }
                    conversation.messages.push(Message {
                        role: "user".to_string(),
                        content: evidence_message,
                    });
                }
                snapshot.insert("status".to_string(), json!("fallback"));
                snapshot.insert(
                    "fallback_reason".to_string(),
                    json!("loop_ended_without_final"),
                );
                trace.save(agent_id, &snapshot);
                Ok((None, "loop_ended_without_final".to_string()))
            })();
        match outcome {
            Ok(pair) => pair,
            Err(error) => {
                // Loop failures fall back to the initial review by contract.
                let reason = error.to_string();
                snapshot.insert("status".to_string(), json!("fallback"));
                snapshot.insert("fallback_reason".to_string(), json!(reason.clone()));
                trace.save(agent_id, &snapshot);
                let mut event = serde_json::Map::new();
                event.insert("agent_id".to_string(), json!(agent_id));
                event.insert("stage".to_string(), json!(stage));
                event.insert("reason".to_string(), json!(reason.clone()));
                trace.log_event("review_agent_fallback", event);
                (None, reason)
            }
        }
    }

    /// Update one turn's status + one field.
    fn mark_turn(
        snapshot: &mut serde_json::Map<String, serde_json::Value>,
        turn_number: i64,
        status: &str,
        field: Option<(&str, serde_json::Value)>,
    ) {
        if let Some(turns) = snapshot.get_mut("turns").and_then(|v| v.as_array_mut()) {
            if let Some(turn) = turns.iter_mut().find_map(|turn| {
                if turn.get("turn").and_then(|v| v.as_i64()) == Some(turn_number) {
                    turn.as_object_mut()
                } else {
                    None
                }
            }) {
                turn.insert("status".to_string(), json!(status));
                if let Some((key, value)) = field {
                    turn.insert(key.to_string(), value);
                }
            }
        }
    }
}

/// Short error kind for trace errors (stable strings, no internals).
fn error_kind(error: &Error) -> &'static str {
    match error {
        Error::Config(_) => "Config",
        Error::Store(_) => "Store",
        Error::Io { .. } => "Io",
        Error::NotFound { .. } => "NotFound",
        Error::Provider(_) => "Provider",
        Error::Stopped(_) => "Stopped",
        Error::Conflict(_) => "Conflict",
        Error::Unsupported(_) => "Unsupported",
    }
}

/// Verify one leaf block with evidence (oracle `ReviewAgentLoop`).
pub struct ReviewAgentLoop<'a> {
    /// Operation id (`review.verify` or `autofix.verify`).
    pub operation: String,
    /// Shared agent state (review phase).
    pub agent: Agent,
    /// Evidence index.
    pub evidence: &'a mut BookEvidenceIndex,
    /// Max evidence rounds.
    pub max_rounds: i64,
    /// Inference fingerprint.
    pub inference: serde_json::Value,
}

impl<'a> ReviewAgentLoop<'a> {
    /// Bind languages + plan + evidence.
    pub fn new(
        src: &str,
        tgt: &str,
        plan: crate::i18n::policy::PolicyPlan,
        config: &Config,
        evidence: &'a mut BookEvidenceIndex,
        operation: &str,
    ) -> Result<Self> {
        let snapshot =
            crate::llm::routing::inference_snapshot(&config.llm, &[operation.to_string()])?;
        Ok(Self {
            operation: operation.to_string(),
            agent: Agent::new(src, tgt, plan, config),
            evidence,
            max_rounds: config.pipeline.review_agent_max_evidence_rounds,
            inference: serde_json::Value::Object(snapshot.into_iter().collect()),
        })
    }

    /// Normalize consistency claims (oracle `_consistency`).
    pub fn consistency(
        value: &serde_json::Value,
    ) -> Result<serde_json::Map<String, serde_json::Value>, ReviewLoopProtocolError> {
        let fail = || ReviewLoopProtocolError {
            reason: "invalid_consistency".to_string(),
        };
        if value.is_null() {
            return Ok(serde_json::Map::new());
        }
        let obj = value.as_object().ok_or_else(fail)?;
        if obj.is_empty() {
            return Ok(serde_json::Map::new());
        }
        let kind = clean_text(obj.get("kind").unwrap_or(&json!(null)));
        let subject = clean_text(obj.get("subject_source").unwrap_or(&json!(null)));
        let proposed = clean_text(obj.get("proposed_value").unwrap_or(&json!(null)));
        if kind.is_empty() && subject.is_empty() && proposed.is_empty() {
            return Ok(serde_json::Map::new());
        }
        if !CONSISTENCY_KINDS.contains(&kind.as_str()) || subject.is_empty() || proposed.is_empty()
        {
            return Err(fail());
        }
        let mut out = serde_json::Map::new();
        out.insert("kind".to_string(), json!(kind));
        out.insert("subject_source".to_string(), json!(subject));
        out.insert("proposed_value".to_string(), json!(proposed));
        Ok(out)
    }

    /// Run bounded block-level evidence review (oracle `review_chunk`).
    #[allow(clippy::too_many_arguments)]
    pub fn review_chunk(
        &mut self,
        client: &mut dyn ModelClient,
        trace: &mut dyn ReviewTrace,
        chapter: i64,
        chunk_base: i64,
        sources: &[String],
        targets: &[String],
        initial_issues: &[serde_json::Map<String, serde_json::Value>],
        review_round: Option<i64>,
    ) -> crate::review_models::ReviewLoopOutcome {
        let mut candidates: Vec<serde_json::Map<String, serde_json::Value>> = Vec::new();
        for (ordinal, issue) in initial_issues.iter().enumerate() {
            let mut candidate = issue.clone();
            candidate.insert(
                "candidate_id".to_string(),
                json!(review_candidate_id(
                    chapter,
                    chunk_base,
                    ordinal as i64,
                    review_round
                )),
            );
            candidates.push(candidate);
        }
        let prefix = review_round
            .map(|round| format!("r{round}-"))
            .unwrap_or_default();
        let agent_id = format!(
            "{prefix}chunk-ch{chapter}-base{chunk_base}-n{}",
            sources.len()
        );
        let mut started = serde_json::Map::new();
        started.insert("agent_id".to_string(), json!(agent_id.clone()));
        started.insert("chapter".to_string(), json!(chapter));
        started.insert("chunk_base".to_string(), json!(chunk_base));
        started.insert("segment_count".to_string(), json!(sources.len() as i64));
        started.insert(
            "candidate_count".to_string(),
            json!(candidates.len() as i64),
        );
        trace.log_event("review_agent_started", started);
        let mut extra = BTreeMap::new();
        extra.insert(
            "max_evidence_rounds".to_string(),
            self.max_rounds.to_string(),
        );
        let system = self
            .agent
            .render("review_agent_system", &extra)
            .unwrap_or_default();
        let mut current_refs: BTreeMap<usize, String> = BTreeMap::new();
        for local in 0..sources.len() {
            if let Some(reference) = self
                .evidence
                .segment_ref(chapter, chunk_base + local as i64)
            {
                current_refs.insert(local, reference.reference());
            }
        }
        let refs: Vec<String> = (0..sources.len())
            .map(|index| current_refs.get(&index).cloned().unwrap_or_default())
            .collect();
        let mut extra = BTreeMap::new();
        extra.insert("chapter".to_string(), chapter.to_string());
        extra.insert(
            "last_index".to_string(),
            sources.len().saturating_sub(1).to_string(),
        );
        extra.insert(
            "pairs".to_string(),
            numbered_pairs_with_refs(sources, targets, &refs),
        );
        extra.insert(
            "segment_refs_json".to_string(),
            json!(current_refs
                .iter()
                .map(|(index, reference)| json!({"index": *index as i64, "ref": reference}))
                .collect::<Vec<_>>())
            .to_string(),
        );
        extra.insert(
            "candidates_json".to_string(),
            serde_json::Value::Array(
                candidates
                    .iter()
                    .map(|c| serde_json::Value::Object(c.clone()))
                    .collect(),
            )
            .to_string(),
        );
        let user = self
            .agent
            .render("review_agent_user", &extra)
            .unwrap_or_default();
        let allowed_refs: BTreeSet<String> = current_refs.values().cloned().collect();
        let issue_types = [
            "missing",
            "added",
            "mistranslation",
            "terminology",
            "pronoun",
        ];
        // Validate the final turn (oracle `validate_final` closure).
        let validate = |data: &serde_json::Map<String, serde_json::Value>,
                        valid_refs: &BTreeSet<String>|
         -> Result<serde_json::Value, ReviewLoopProtocolError> {
            let fail = |reason: &str| ReviewLoopProtocolError {
                reason: reason.to_string(),
            };
            let decisions = data
                .get("decisions")
                .and_then(|v| v.as_array())
                .ok_or_else(|| fail("invalid_final_issue_lists"))?;
            let new_issues = data
                .get("new_issues")
                .and_then(|v| v.as_array())
                .ok_or_else(|| fail("invalid_final_issue_lists"))?;
            let expected: BTreeSet<String> = candidates
                .iter()
                .filter_map(|c| {
                    c.get("candidate_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .collect();
            let mut by_id: BTreeMap<String, &serde_json::Map<String, serde_json::Value>> =
                BTreeMap::new();
            for decision in decisions {
                let decision = decision
                    .as_object()
                    .ok_or_else(|| fail("decision_not_object"))?;
                let candidate_id = clean_text(decision.get("candidate_id").unwrap_or(&json!(null)));
                if candidate_id.is_empty()
                    || by_id.contains_key(&candidate_id)
                    || !expected.contains(&candidate_id)
                {
                    return Err(fail("invalid_candidate_decision"));
                }
                by_id.insert(candidate_id, decision);
            }
            if by_id.keys().cloned().collect::<BTreeSet<_>>() != expected {
                return Err(fail("candidate_decisions_incomplete"));
            }
            let by_candidate: BTreeMap<String, &serde_json::Map<String, serde_json::Value>> =
                candidates
                    .iter()
                    .filter_map(|c| {
                        c.get("candidate_id")
                            .and_then(|v| v.as_str())
                            .map(|id| (id.to_string(), c))
                    })
                    .collect();
            let issue_refs = |index: usize,
                              value: &serde_json::Value,
                              valid: &BTreeSet<String>|
             -> Result<Vec<String>, ReviewLoopProtocolError> {
                let mut refs = validate_evidence_refs(value, valid)?;
                if let Some(current) = current_refs.get(&index) {
                    if !refs.contains(current) {
                        refs.insert(0, current.clone());
                    }
                }
                Ok(refs)
            };
            let mut kept = Vec::new();
            let mut dismissed = Vec::new();
            let mut expected_sorted: Vec<String> = expected.into_iter().collect();
            expected_sorted.sort();
            for candidate_id in expected_sorted {
                let decision = by_id[&candidate_id];
                let candidate = by_candidate[&candidate_id];
                match decision.get("verdict").and_then(|v| v.as_str()) {
                    Some("dismissed") => {
                        let reason = clean_text(decision.get("reason").unwrap_or(&json!(null)));
                        if reason.is_empty() {
                            return Err(fail("dismissal_without_reason"));
                        }
                        let index = candidate.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                        dismissed.push(json!({
                            "candidate_id": candidate_id,
                            "index": index,
                            "type": candidate.get("type"),
                            "detail": candidate.get("detail"),
                            "suggestion": candidate.get("suggestion"),
                            "reason": reason,
                            "evidence_refs": issue_refs(
                                index as usize,
                                decision.get("evidence_refs").unwrap_or(&json!(null)),
                                valid_refs
                            )?,
                        }));
                    }
                    Some("confirmed") => {
                        let detail = clean_text(decision.get("detail").unwrap_or(&json!(null)));
                        let detail = if detail.is_empty() {
                            clean_text(candidate.get("detail").unwrap_or(&json!(null)))
                        } else {
                            detail
                        };
                        let suggestion =
                            clean_text(decision.get("suggestion").unwrap_or(&json!(null)));
                        let suggestion = if suggestion.is_empty() {
                            clean_text(candidate.get("suggestion").unwrap_or(&json!(null)))
                        } else {
                            suggestion
                        };
                        if detail.is_empty() || suggestion.is_empty() {
                            return Err(fail("confirmed_issue_missing_text"));
                        }
                        let index = candidate.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                        kept.push(json!({
                            "index": index,
                            "type": candidate.get("type"),
                            "detail": detail,
                            "suggestion": suggestion,
                            "origin": "initial",
                            "candidate_id": candidate_id,
                            "consistency": Self::consistency(
                                decision.get("consistency").unwrap_or(&json!(null))
                            )?,
                            "evidence_refs": issue_refs(
                                index as usize,
                                decision.get("evidence_refs").unwrap_or(&json!(null)),
                                valid_refs
                            )?,
                        }));
                    }
                    _ => return Err(fail("invalid_candidate_verdict")),
                }
            }
            let limit = 50.min(4.max(sources.len() as i64 * 2));
            if new_issues.len() as i64 > limit {
                return Err(fail("too_many_new_issues"));
            }
            for issue in new_issues {
                let issue = issue
                    .as_object()
                    .ok_or_else(|| fail("new_issue_not_object"))?;
                let index = issue.get("index").and_then(|v| v.as_i64());
                if issue.get("index").is_some_and(|v| v.is_boolean())
                    || index.is_none_or(|i| i < 0 || i >= sources.len() as i64)
                {
                    return Err(fail("new_issue_outside_chunk"));
                }
                let index = index.unwrap();
                let issue_type = issue
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let detail = clean_text(issue.get("detail").unwrap_or(&json!(null)));
                let suggestion = clean_text(issue.get("suggestion").unwrap_or(&json!(null)));
                if !issue_types.contains(&issue_type) || detail.is_empty() || suggestion.is_empty()
                {
                    return Err(fail("invalid_new_issue"));
                }
                kept.push(json!({
                    "index": index,
                    "type": issue_type,
                    "detail": detail,
                    "suggestion": suggestion,
                    "origin": "agent",
                    "consistency": Self::consistency(
                        issue.get("consistency").unwrap_or(&json!(null))
                    )?,
                    "evidence_refs": issue_refs(
                        index as usize,
                        issue.get("evidence_refs").unwrap_or(&json!(null)),
                        valid_refs
                    )?,
                }));
            }
            Ok(json!({"issues": kept, "dismissed": dismissed}))
        };
        // Borrow split: reborrow the evidence reference for the loop.
        let operation = self.operation.clone();
        let max_rounds = self.max_rounds;
        let inference = self.inference.clone();
        let mut action = ReviewActionLoop {
            max_rounds,
            inference,
            evidence: &mut *self.evidence,
        };
        let (result, reason) = action.run(
            client,
            trace,
            &agent_id,
            &system,
            &user,
            &operation,
            allowed_refs.clone(),
            &validate,
        );
        let Some(result) = result else {
            let fallback: Vec<serde_json::Map<String, serde_json::Value>> = initial_issues
                .iter()
                .map(|issue| {
                    let mut item = issue.clone();
                    item.insert("origin".to_string(), json!("initial"));
                    item.insert("agent_fallback".to_string(), json!(true));
                    item.insert("fallback_reason".to_string(), json!(reason.clone()));
                    let refs = issue
                        .get("index")
                        .and_then(|v| v.as_i64())
                        .and_then(|index| current_refs.get(&(index as usize)).cloned())
                        .map(|reference| vec![json!(reference)])
                        .unwrap_or_default();
                    item.insert("evidence_refs".to_string(), serde_json::Value::Array(refs));
                    item
                })
                .collect();
            return crate::review_models::ReviewLoopOutcome {
                issues: fallback,
                dismissed: Vec::new(),
                fallback_reason: reason,
            };
        };
        let issues = result
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_object().cloned())
                    .collect()
            })
            .unwrap_or_default();
        let dismissed = result
            .get("dismissed")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_object().cloned())
                    .collect()
            })
            .unwrap_or_default();
        crate::review_models::ReviewLoopOutcome {
            issues,
            dismissed,
            fallback_reason: String::new(),
        }
    }
}
