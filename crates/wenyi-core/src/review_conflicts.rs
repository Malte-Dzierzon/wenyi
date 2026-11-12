//! Review conflicts: normalization, grouping, arbitration.
//!
//! Oracle: `review/conflicts.py` + `agents/review_arbiter.py`. Pure
//! normalization/grouping plus the read-only arbiter loop. Failures keep
//! all issues and mark the conflict unresolved (never drop evidence).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::config::Config;
use crate::i18n::policy::PolicyPlan;
use crate::pipeline_agents::{Agent, ModelClient};
use crate::review_evidence::BookEvidenceIndex;
use crate::review_loop::{ReviewActionLoop, ReviewLoopProtocolError, ReviewTrace};
use crate::review_models::{clean_text, normalize_value, review_issue_key, CONSISTENCY_KINDS};
use crate::Result;

/// Max conflicting values per arbitration (oracle cap).
pub const MAX_ARBITRATION_PROPOSALS: usize = 32;
/// Max arbitration prompt bytes (oracle cap).
pub const MAX_ARBITRATION_PAYLOAD_BYTES: usize = 96_000;
/// Per-sample text limit (oracle cap).
pub const ARBITRATION_SAMPLE_TEXT_LIMIT: usize = 1500;

/// Normalize issues + assign round-local ids and stable keys (oracle
/// `normalize_review_issues`).
pub fn normalize_review_issues(
    issues: &[serde_json::Map<String, serde_json::Value>],
    evidence: &BookEvidenceIndex,
) -> Vec<serde_json::Map<String, serde_json::Value>> {
    let mut sorted: Vec<serde_json::Map<String, serde_json::Value>> = issues.to_vec();
    sorted.sort_by(|a, b| {
        let key = |issue: &serde_json::Map<String, serde_json::Value>| {
            (
                issue.get("chapter").and_then(|v| v.as_i64()).unwrap_or(-1),
                issue.get("index").and_then(|v| v.as_i64()).unwrap_or(-1),
                issue
                    .get("_chunk_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                issue
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            )
        };
        key(a).cmp(&key(b))
    });
    let mut prepared = Vec::new();
    let mut seen_keys = BTreeSet::new();
    for issue in sorted {
        let mut item = issue;
        if let Some(consistency) = item.get("consistency").and_then(|v| v.as_object()).cloned() {
            let kind = clean_text(consistency.get("kind").unwrap_or(&json!(null)));
            let subject = clean_text(consistency.get("subject_source").unwrap_or(&json!(null)));
            let proposed = clean_text(consistency.get("proposed_value").unwrap_or(&json!(null)));
            if CONSISTENCY_KINDS.contains(&kind.as_str())
                && !subject.is_empty()
                && !proposed.is_empty()
            {
                let (term, ambiguous) = evidence.canonical_term(&subject);
                let mut normalized = serde_json::Map::new();
                normalized.insert("kind".to_string(), json!(kind));
                normalized.insert("subject_source".to_string(), json!(subject));
                if !ambiguous.is_empty() {
                    normalized.insert("canonical_source".to_string(), json!(""));
                    normalized.insert("proposed_value".to_string(), json!(proposed));
                    normalized.insert("ambiguous_sources".to_string(), json!(ambiguous));
                    normalized.insert("auto_arbitration".to_string(), json!(false));
                } else {
                    let canonical = term
                        .map(|pos| evidence.terms[pos].source.clone())
                        .unwrap_or_else(|| subject.clone());
                    let key = if term.is_some() {
                        format!("glossary:{canonical}")
                    } else {
                        normalize_value(&canonical)
                    };
                    let key = format!("{kind}:{key}");
                    normalized.insert("canonical_source".to_string(), json!(canonical));
                    normalized.insert("key".to_string(), json!(key));
                    normalized.insert("proposed_value".to_string(), json!(proposed));
                }
                item.insert(
                    "consistency".to_string(),
                    serde_json::Value::Object(normalized),
                );
            } else {
                item.insert("consistency".to_string(), json!({}));
            }
        }
        let issue_key = review_issue_key(&item);
        if seen_keys.contains(&issue_key) {
            continue;
        }
        seen_keys.insert(issue_key.clone());
        item.insert("issue_key".to_string(), json!(issue_key));
        item.insert(
            "issue_id".to_string(),
            json!(format!("review-{:05}", prepared.len() + 1)),
        );
        prepared.push(item);
    }
    prepared
}

/// Find mutually exclusive values for one subject (oracle
/// `build_conflict_groups`).
pub fn build_conflict_groups(
    issues: &[serde_json::Map<String, serde_json::Value>],
) -> Vec<serde_json::Map<String, serde_json::Value>> {
    let mut grouped: BTreeMap<String, Vec<serde_json::Map<String, serde_json::Value>>> =
        BTreeMap::new();
    for issue in issues {
        let Some(consistency) = issue.get("consistency").and_then(|v| v.as_object()) else {
            continue;
        };
        let key = clean_text(consistency.get("key").unwrap_or(&json!(null)));
        let proposed = clean_text(consistency.get("proposed_value").unwrap_or(&json!(null)));
        if !key.is_empty() && !proposed.is_empty() {
            grouped.entry(key).or_default().push(issue.clone());
        }
    }
    let mut conflicts = Vec::new();
    for (key, group) in grouped {
        let chunks: BTreeSet<String> = group
            .iter()
            .map(|issue| {
                issue
                    .get("_chunk_id")
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            })
            .collect();
        let mut values: BTreeSet<String> = group
            .iter()
            .map(|issue| {
                issue
                    .get("consistency")
                    .and_then(|v| v.as_object())
                    .map(|obj| {
                        normalize_value(&clean_text(
                            obj.get("proposed_value").unwrap_or(&json!(null)),
                        ))
                    })
                    .unwrap_or_default()
            })
            .collect();
        values.remove("");
        if chunks.len() < 2 || values.len() < 2 {
            continue;
        }
        let first = group
            .iter()
            .map(|issue| {
                (
                    issue.get("chapter").and_then(|v| v.as_i64()).unwrap_or(-1),
                    issue.get("index").and_then(|v| v.as_i64()).unwrap_or(-1),
                )
            })
            .min()
            .unwrap_or((-1, -1));
        let mut conflict = serde_json::Map::new();
        conflict.insert("consistency_key".to_string(), json!(key));
        conflict.insert(
            "issues".to_string(),
            serde_json::Value::Array(group.into_iter().map(serde_json::Value::Object).collect()),
        );
        conflict.insert("first_position".to_string(), json!([first.0, first.1]));
        conflicts.push(conflict);
    }
    conflicts.sort_by(|a, b| {
        let key = |conflict: &serde_json::Map<String, serde_json::Value>| {
            (
                conflict
                    .get("first_position")
                    .and_then(|v| v.as_array())
                    .map(|pair| {
                        (
                            pair.first().and_then(|v| v.as_i64()).unwrap_or(-1),
                            pair.get(1).and_then(|v| v.as_i64()).unwrap_or(-1),
                        )
                    })
                    .unwrap_or((-1, -1)),
                conflict
                    .get("consistency_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            )
        };
        key(a).cmp(&key(b))
    });
    for (ordinal, conflict) in conflicts.iter_mut().enumerate() {
        conflict.insert(
            "conflict_id".to_string(),
            json!(format!("review-conflict-{:04}", ordinal + 1)),
        );
    }
    conflicts
}

/// Apply arbitrations to the recommendation view (oracle
/// `apply_review_arbitrations`). Returns `(final, superseded)`.
/// Final issues + superseded rows.
pub type ArbitrationOutcome = (
    Vec<serde_json::Map<String, serde_json::Value>>,
    Vec<serde_json::Map<String, serde_json::Value>>,
);
pub fn apply_review_arbitrations(
    issues: &[serde_json::Map<String, serde_json::Value>],
    arbitrations: &[serde_json::Map<String, serde_json::Value>],
) -> ArbitrationOutcome {
    let mut by_id: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for issue in issues {
        if let Some(id) = issue.get("issue_id").and_then(|v| v.as_str()) {
            by_id.insert(id.to_string(), issue.clone());
        }
    }
    let mut superseded_rows = Vec::new();
    for arbitration in arbitrations {
        let conflict_id = clean_text(arbitration.get("conflict_id").unwrap_or(&json!(null)));
        let status = arbitration
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let recommended = clean_text(arbitration.get("recommended_value").unwrap_or(&json!(null)));
        let reason = clean_text(arbitration.get("reason").unwrap_or(&json!(null)));
        let annotation = json!({
            "conflict_id": conflict_id,
            "status": status,
            "recommended_value": recommended,
            "reason": reason,
        });
        if status == "suggested" {
            let rejected: Vec<String> = arbitration
                .get("rejected_issue_ids")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            for issue_id in rejected {
                let Some(issue) = by_id.get_mut(&issue_id) else {
                    continue;
                };
                let mut issue_annotation = annotation.as_object().cloned().unwrap_or_default();
                issue_annotation.insert("action".to_string(), json!("rewritten"));
                superseded_rows.push({
                    let mut row = issue.clone();
                    row.insert(
                        "arbitration".to_string(),
                        serde_json::Value::Object(issue_annotation.clone()),
                    );
                    row
                });
                let previous_detail = clean_text(issue.get("detail").unwrap_or(&json!(null)));
                let previous_suggestion =
                    clean_text(issue.get("suggestion").unwrap_or(&json!(null)));
                issue.insert("pre_arbitration_detail".to_string(), json!(previous_detail));
                issue.insert(
                    "pre_arbitration_suggestion".to_string(),
                    json!(previous_suggestion),
                );
                issue.insert(
                    "detail".to_string(),
                    json!(format!(
                        "Final arbitration requires the expression here to use “{recommended}” consistently."
                    )),
                );
                issue.insert(
                    "suggestion".to_string(),
                    json!(format!(
                        "Use “{recommended}” consistently for this expression as determined by final arbitration."
                    )),
                );
                if let Some(consistency) = issue
                    .get("consistency")
                    .and_then(|v| v.as_object())
                    .cloned()
                {
                    let mut updated = consistency;
                    updated.insert("proposed_value".to_string(), json!(recommended));
                    issue.insert(
                        "consistency".to_string(),
                        serde_json::Value::Object(updated),
                    );
                }
                issue.insert(
                    "arbitration".to_string(),
                    serde_json::Value::Object(issue_annotation),
                );
            }
            let supported: Vec<String> = arbitration
                .get("supported_issue_ids")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            for issue_id in supported {
                if let Some(issue) = by_id.get_mut(&issue_id) {
                    issue.insert("arbitration".to_string(), annotation.clone());
                }
            }
        } else if status == "unresolved" {
            let ids: Vec<String> = arbitration
                .get("issue_ids")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            for issue_id in ids {
                if let Some(issue) = by_id.get_mut(&issue_id) {
                    issue.insert("arbitration".to_string(), annotation.clone());
                }
            }
        }
    }
    let mut order: BTreeMap<String, usize> = BTreeMap::new();
    for (position, issue) in issues.iter().enumerate() {
        if let Some(id) = issue.get("issue_id").and_then(|v| v.as_str()) {
            order.insert(id.to_string(), position);
        }
    }
    let rank = |issue: &serde_json::Map<String, serde_json::Value>| {
        issue
            .get("issue_id")
            .and_then(|v| v.as_str())
            .and_then(|id| order.get(id).copied())
            .unwrap_or(usize::MAX)
    };
    let mut final_issues: Vec<serde_json::Map<String, serde_json::Value>> =
        by_id.into_values().collect();
    final_issues.sort_by_key(rank);
    superseded_rows.sort_by_key(|issue| {
        issue
            .get("issue_id")
            .and_then(|v| v.as_str())
            .and_then(|id| order.get(id).copied())
            .unwrap_or(usize::MAX)
    });
    (final_issues, superseded_rows)
}

/// Read-only conflict arbiter (oracle `ReviewConflictArbiter`).
pub struct ReviewConflictArbiter<'a> {
    /// Shared agent state (review phase).
    pub agent: Agent,
    /// Evidence index.
    pub evidence: &'a mut BookEvidenceIndex,
    /// Max evidence rounds.
    pub max_rounds: i64,
    /// Inference fingerprint.
    pub inference: serde_json::Value,
}

impl<'a> ReviewConflictArbiter<'a> {
    /// Bind languages + plan + evidence.
    pub fn new(
        src: &str,
        tgt: &str,
        plan: PolicyPlan,
        config: &Config,
        evidence: &'a mut BookEvidenceIndex,
    ) -> Result<Self> {
        let snapshot = crate::llm::routing::inference_snapshot(
            &config.llm,
            &["review.arbitrate".to_string()],
        )?;
        Ok(Self {
            agent: Agent::new(src, tgt, plan, config),
            evidence,
            max_rounds: config.pipeline.review_agent_max_evidence_rounds,
            inference: serde_json::Value::Object(snapshot.into_iter().collect()),
        })
    }

    /// Arbitrate one conflict; unresolved on any failure.
    pub fn arbitrate(
        &mut self,
        client: &mut dyn ModelClient,
        trace: &mut dyn ReviewTrace,
        conflict: &serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let conflict_id = conflict
            .get("conflict_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let issues: Vec<serde_json::Map<String, serde_json::Value>> = conflict
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_object().cloned())
                    .collect()
            })
            .unwrap_or_default();
        let issue_ids: Vec<String> = issues
            .iter()
            .filter_map(|issue| {
                issue
                    .get("issue_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();
        let unresolved = |trace: &mut dyn ReviewTrace, reason: String, refs: BTreeSet<String>| {
            let mut data = serde_json::Map::new();
            data.insert("conflict_id".to_string(), json!(conflict_id.clone()));
            data.insert("issue_count".to_string(), json!(issue_ids.len() as i64));
            data.insert("reason".to_string(), json!(reason.clone()));
            trace.log_event("review_arbitration_unresolved", data);
            let mut out = serde_json::Map::new();
            out.insert("conflict_id".to_string(), json!(conflict_id.clone()));
            out.insert(
                "consistency_key".to_string(),
                conflict
                    .get("consistency_key")
                    .cloned()
                    .unwrap_or(json!("")),
            );
            out.insert("issue_ids".to_string(), json!(issue_ids.clone()));
            out.insert("status".to_string(), json!("unresolved"));
            out.insert("recommended_value".to_string(), json!(""));
            out.insert("reason".to_string(), json!(reason));
            out.insert("supported_issue_ids".to_string(), json!(issue_ids.clone()));
            out.insert("rejected_issue_ids".to_string(), json!([]));
            out.insert(
                "evidence_refs".to_string(),
                json!(refs.into_iter().collect::<Vec<_>>()),
            );
            out
        };
        let mut proposal_groups: BTreeMap<String, Vec<serde_json::Map<String, serde_json::Value>>> =
            BTreeMap::new();
        for issue in &issues {
            let proposed = issue
                .get("consistency")
                .and_then(|v| v.as_object())
                .map(|obj| clean_text(obj.get("proposed_value").unwrap_or(&json!(null))))
                .unwrap_or_default();
            proposal_groups
                .entry(normalize_value(&proposed))
                .or_default()
                .push(issue.clone());
        }
        if proposal_groups.len() > MAX_ARBITRATION_PROPOSALS {
            return unresolved(
                trace,
                format!(
                    "Too many conflicting values ({}) for selective arbitration.",
                    proposal_groups.len()
                ),
                BTreeSet::new(),
            );
        }
        let clip = |text: &str| {
            text.chars()
                .take(ARBITRATION_SAMPLE_TEXT_LIMIT)
                .collect::<String>()
        };
        let mut sampled_refs = BTreeSet::new();
        let mut proposal_rows = Vec::new();
        for grouped in proposal_groups.values() {
            let mut positions = vec![0, (grouped.len() - 1) / 2, grouped.len() - 1];
            positions.dedup();
            let mut samples = Vec::new();
            for position in positions {
                let issue = &grouped[position];
                let chapter = issue.get("chapter").and_then(|v| v.as_i64()).unwrap_or(0);
                let index = issue.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
                let segment = self.evidence.segment_ref(chapter, index);
                if let Some(segment) = segment {
                    sampled_refs.insert(segment.reference());
                }
                samples.push(json!({
                    "issue_id": issue.get("issue_id"),
                    "chapter": issue.get("chapter"),
                    "index": issue.get("index"),
                    "type": issue.get("type"),
                    "detail": clip(&clean_text(issue.get("detail").unwrap_or(&json!(null)))),
                    "suggestion": clip(&clean_text(issue.get("suggestion").unwrap_or(&json!(null)))),
                    "segment_ref": segment.map(|s| s.reference()).unwrap_or_default(),
                    "source": segment.map(|s| clip(&s.source)).unwrap_or_default(),
                    "target": segment.map(|s| clip(&s.target)).unwrap_or_default(),
                }));
            }
            proposal_rows.push(json!({
                "proposed_value": grouped[0]
                    .get("consistency")
                    .and_then(|v| v.as_object())
                    .and_then(|obj| obj.get("proposed_value"))
                    .cloned()
                    .unwrap_or(json!("")),
                "issue_count": grouped.len() as i64,
                "samples": samples,
            }));
        }
        let compact = json!({
            "conflict_id": conflict_id,
            "consistency_key": conflict.get("consistency_key"),
            "issue_count": issue_ids.len() as i64,
            "proposals": proposal_rows,
        });
        let compact_json = serde_json::to_string_pretty(&compact).unwrap_or_default();
        if compact_json.len() > MAX_ARBITRATION_PAYLOAD_BYTES {
            return unresolved(
                trace,
                "Selective arbitration samples still exceed the input size limit.".to_string(),
                sampled_refs,
            );
        }
        let mut extra = std::collections::BTreeMap::new();
        extra.insert(
            "max_evidence_rounds".to_string(),
            self.max_rounds.to_string(),
        );
        let system = self
            .agent
            .render("review_arbiter_system", &extra)
            .unwrap_or_default();
        let mut extra = std::collections::BTreeMap::new();
        extra.insert("conflict_json".to_string(), compact_json);
        let user = self
            .agent
            .render("review_arbiter_user", &extra)
            .unwrap_or_default();
        let allowed_refs = sampled_refs.clone();
        let validate = |data: &serde_json::Map<String, serde_json::Value>,
                        valid_refs: &BTreeSet<String>|
         -> Result<serde_json::Value, ReviewLoopProtocolError> {
            let fail = |reason: &str| ReviewLoopProtocolError {
                reason: reason.to_string(),
            };
            if data.get("conflict_id").and_then(|v| v.as_str()) != Some(conflict_id.as_str()) {
                return Err(fail("conflict_id_mismatch"));
            }
            if data.contains_key("supported_issue_ids") || data.contains_key("rejected_issue_ids") {
                return Err(fail("arbitration_issue_ids_must_be_omitted"));
            }
            let status = data
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if status != "suggested" && status != "unresolved" {
                return Err(fail("invalid_arbitration_status"));
            }
            let recommended = clean_text(data.get("recommended_value").unwrap_or(&json!(null)));
            let reason = clean_text(data.get("reason").unwrap_or(&json!(null)));
            if status == "suggested" && recommended.is_empty() {
                return Err(fail("suggestion_without_value"));
            }
            if reason.is_empty() {
                return Err(fail("arbitration_without_reason"));
            }
            let (recommended, supported, rejected) = if status == "suggested" {
                let normalized = normalize_value(&recommended);
                let Some(group) = proposal_groups.get(&normalized) else {
                    return Err(fail("recommended_value_not_proposed"));
                };
                let canonical = clean_text(
                    group[0]
                        .get("consistency")
                        .and_then(|v| v.as_object())
                        .and_then(|obj| obj.get("proposed_value"))
                        .unwrap_or(&json!(null)),
                );
                let supported: Vec<String> = issues
                    .iter()
                    .filter(|issue| {
                        normalize_value(&clean_text(
                            issue
                                .get("consistency")
                                .and_then(|v| v.as_object())
                                .and_then(|obj| obj.get("proposed_value"))
                                .unwrap_or(&json!(null)),
                        )) == normalized
                    })
                    .filter_map(|issue| {
                        issue
                            .get("issue_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .collect();
                let supported_set: BTreeSet<String> = supported.iter().cloned().collect();
                let rejected: Vec<String> = issue_ids
                    .iter()
                    .filter(|id| !supported_set.contains(*id))
                    .cloned()
                    .collect();
                (canonical, supported, rejected)
            } else {
                (String::new(), issue_ids.clone(), Vec::new())
            };
            let refs = crate::review_loop::validate_evidence_refs(
                data.get("evidence_refs").unwrap_or(&json!(null)),
                valid_refs,
            )?;
            Ok(json!({
                "conflict_id": conflict_id,
                "consistency_key": conflict.get("consistency_key"),
                "issue_ids": issue_ids,
                "status": status,
                "recommended_value": recommended,
                "reason": reason,
                "supported_issue_ids": supported,
                "rejected_issue_ids": rejected,
                "evidence_refs": refs,
            }))
        };
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
            &format!("arbiter-{conflict_id}"),
            &system,
            &user,
            "review.arbitrate",
            allowed_refs.clone(),
            &validate,
        );
        match result.and_then(|v| v.as_object().cloned()) {
            Some(arbitration) => arbitration,
            None => unresolved(
                trace,
                format!("Arbitration agent did not complete: {reason}"),
                allowed_refs,
            ),
        }
    }
}
