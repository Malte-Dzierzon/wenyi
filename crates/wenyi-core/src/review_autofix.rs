//! Autofix publication: recoverable plans, verification, formal writes.
//!
//! Oracle: `pipeline/autofix_*.py` + `review_autofix.py` +
//! `review/autofix_models.py`. The only formal-target writer after review:
//! candidates accumulate in memory, the index persists with
//! `status=applying` *before* any formal write, then optimistic
//! before-hash checks gate per-segment publication. Resume distinguishes
//! `applying` (re-apply) from `completed`/`partial` (re-finish).

use std::collections::BTreeMap;

use rusqlite::Connection;
use serde_json::json;

use crate::config::Config;
use crate::glossary::GlossaryTerm;
use crate::model::{BookId, ProjectId};
use crate::pipeline::Progress;
use crate::pipeline_agents::{ModelClient, ReviewFixer};
use crate::review::{load_review_chapters, ReviewChapter, TraceStore};
use crate::review_evidence::BookEvidenceIndex;
use crate::review_loop::{ReviewAgentLoop, ReviewTrace};
use crate::review_models::{review_candidate_id, text_hash};
use crate::{store, Result};

/// Next autofix record id for a candidate list.
pub fn next_record_id(records: &[serde_json::Map<String, serde_json::Value>]) -> String {
    format!("autofix-{:05}", records.len() + 1)
}

/// Append one candidate record (oracle `AutofixCandidates.add`).
#[allow(clippy::too_many_arguments)]
pub fn add_record(
    records: &mut Vec<serde_json::Map<String, serde_json::Value>>,
    chapter: i64,
    index: i64,
    segment_ref: &str,
    origin: &str,
    before: &str,
    after: Option<&str>,
    issue_keys: &[String],
    issue_ids: &[String],
    review_result: &str,
    status: &str,
    reason: &str,
    source_change: Option<&serde_json::Map<String, serde_json::Value>>,
    related_issues: &[serde_json::Map<String, serde_json::Value>],
    artifacts: &[String],
) -> serde_json::Map<String, serde_json::Value> {
    let mut record = serde_json::Map::new();
    record.insert("record_id".to_string(), json!(next_record_id(records)));
    record.insert("chapter".to_string(), json!(chapter));
    record.insert("index".to_string(), json!(index));
    record.insert("segment_ref".to_string(), json!(segment_ref));
    record.insert("origin".to_string(), json!(origin));
    record.insert("before".to_string(), json!(before));
    record.insert("before_hash".to_string(), json!(text_hash(before)));
    record.insert(
        "after".to_string(),
        after
            .map(|text| json!(text))
            .unwrap_or(serde_json::Value::Null),
    );
    record.insert(
        "after_hash".to_string(),
        after
            .map(|text| json!(text_hash(text)))
            .unwrap_or(serde_json::Value::Null),
    );
    let mut keys: Vec<String> = issue_keys
        .iter()
        .filter(|key| !key.is_empty())
        .cloned()
        .collect();
    keys.sort();
    keys.dedup();
    record.insert("issue_keys".to_string(), json!(keys));
    let mut ids: Vec<String> = issue_ids
        .iter()
        .filter(|id| !id.is_empty())
        .cloned()
        .collect();
    ids.sort();
    ids.dedup();
    record.insert("issue_ids".to_string(), json!(ids));
    record.insert("review_result".to_string(), json!(review_result));
    record.insert("status".to_string(), json!(status));
    if !reason.is_empty() {
        record.insert("reason".to_string(), json!(reason));
    }
    if let Some(change) = source_change {
        record.insert(
            "source_change".to_string(),
            serde_json::Value::Object(change.clone()),
        );
    }
    if !related_issues.is_empty() {
        record.insert(
            "related_issues".to_string(),
            json!(related_issues
                .iter()
                .map(|issue| json!(issue))
                .collect::<Vec<_>>()),
        );
    }
    if !artifacts.is_empty() {
        let mut sorted = artifacts.to_vec();
        sorted.sort();
        sorted.dedup();
        record.insert("artifacts".to_string(), json!(sorted));
    }
    records.push(record.clone());
    record
}

/// Pin the autofix inference identity (oracle `prepare_identity`).
pub fn prepare_identity(
    conn: &Connection,
    review_id: &str,
    round: i64,
    llm: &crate::config::LlmConfig,
) -> Result<BTreeMap<String, serde_json::Value>> {
    let inference = crate::llm::routing::inference_snapshot(
        llm,
        &["autofix.verify".to_string(), "autofix.fix".to_string()],
    )?;
    let inference_value = serde_json::Value::Object(
        inference
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    if let Some(existing) = store::load_review_trace(conn, review_id, round, "autofix/plan.json")? {
        let plan: serde_json::Value = serde_json::from_str(&existing).unwrap_or_default();
        if plan.get("inference") != Some(&inference_value) {
            return Err(crate::Error::Config(
                "Autofix planning models changed. Restore the prior routes to finish this review."
                    .to_string(),
            ));
        }
    }
    let plan = json!({"inference": inference_value});
    store::save_review_trace(
        conn,
        review_id,
        round,
        "autofix/plan.json",
        &plan.to_string(),
    )?;
    Ok(inference)
}

/// Prepared candidates: records, overrides, issue-group count.
pub type CandidateSet = (
    Vec<serde_json::Map<String, serde_json::Value>>,
    BTreeMap<(i64, i64), String>,
    i64,
);

/// Verify one issue group and propose a patch (oracle `fix_job`).
#[allow(clippy::too_many_arguments)]
pub fn fix_group(
    client: &mut dyn ModelClient,
    trace: &mut dyn ReviewTrace,
    evidence: &mut BookEvidenceIndex,
    all_terms: &[GlossaryTerm],
    style: &str,
    book_synopsis: &str,
    chapter_digests: &BTreeMap<i64, String>,
    src: &str,
    tgt: &str,
    plan: &crate::i18n::policy::PolicyPlan,
    config: &Config,
    fixer_round: i64,
    location: (i64, i64),
    location_issues: &[serde_json::Map<String, serde_json::Value>],
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    out.insert("location".to_string(), json!([location.0, location.1]));
    let Some(segment) = evidence.segment_ref(location.0, location.1).cloned() else {
        out.insert("status".to_string(), json!("failed"));
        out.insert("reason".to_string(), json!("segment_not_found"));
        return out;
    };
    // Localize indices to the singleton chunk for the agent loop.
    let localized: Vec<serde_json::Map<String, serde_json::Value>> = location_issues
        .iter()
        .map(|issue| {
            let mut localized = issue.clone();
            localized.insert("index".to_string(), json!(0));
            localized
        })
        .collect();
    let mut candidates: BTreeMap<String, &serde_json::Map<String, serde_json::Value>> =
        BTreeMap::new();
    for (ordinal, issue) in location_issues.iter().enumerate() {
        candidates.insert(
            review_candidate_id(location.0, location.1, ordinal as i64, Some(fixer_round)),
            issue,
        );
    }
    let mut agent = match ReviewAgentLoop::new(
        src,
        tgt,
        plan.clone(),
        config,
        &mut *evidence,
        "autofix.verify",
    ) {
        Ok(agent) => agent,
        Err(error) => {
            out.insert("status".to_string(), json!("failed"));
            out.insert("reason".to_string(), json!(error.to_string()));
            return out;
        }
    };
    let loop_outcome = agent.review_chunk(
        client,
        trace,
        location.0,
        location.1,
        std::slice::from_ref(&segment.source),
        std::slice::from_ref(&segment.target),
        &localized,
        Some(fixer_round),
    );
    // Restore the reborrowed evidence (the agent drops here).
    if !loop_outcome.fallback_reason.is_empty() {
        out.insert("status".to_string(), json!("failed"));
        out.insert(
            "reason".to_string(),
            json!(format!(
                "review_agent_fallback:{}",
                loop_outcome.fallback_reason
            )),
        );
        return out;
    }
    let mut verified = Vec::new();
    for (ordinal, issue) in loop_outcome.issues.iter().enumerate() {
        let mut mapped = issue.clone();
        let candidate_id = mapped
            .get("candidate_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let (issue_id, issue_key) = match candidates.get(&candidate_id) {
            Some(original) => (
                original
                    .get("issue_id")
                    .or(original.get("issue_key"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&candidate_id)
                    .to_string(),
                original
                    .get("issue_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&candidate_id)
                    .to_string(),
            ),
            None => {
                let identity = json!([
                    location.0,
                    location.1,
                    mapped.get("type"),
                    mapped.get("detail"),
                    mapped.get("suggestion"),
                    ordinal as i64,
                ])
                .to_string();
                let id = format!("autofix-agent-{:.16}", text_hash(&identity));
                (id.clone(), id)
            }
        };
        mapped.insert("issue_id".to_string(), json!(issue_id));
        mapped.insert("issue_key".to_string(), json!(issue_key));
        mapped.insert("chapter".to_string(), json!(location.0));
        mapped.insert("index".to_string(), json!(location.1));
        verified.push(mapped);
    }
    out.insert(
        "verified_issues".to_string(),
        json!(verified
            .iter()
            .map(|issue| json!(issue))
            .collect::<Vec<_>>()),
    );
    out.insert(
        "dismissed".to_string(),
        json!(loop_outcome
            .dismissed
            .iter()
            .map(|issue| json!(issue))
            .collect::<Vec<_>>()),
    );
    if verified.is_empty() {
        out.insert("status".to_string(), json!("dismissed"));
        return out;
    }
    // Fixer context: 4-neighborhood + relevant terms from the snapshot.
    let context_args = json!({"chapter": location.0, "index": location.1, "before": 4, "after": 4});
    let context = evidence.segment_context(context_args.as_object().unwrap());
    let context_segments: Vec<serde_json::Value> = context
        .get("segments")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let nearby_pairs: Vec<(String, String)> = context_segments
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            if obj.get("ref").and_then(|v| v.as_str()) == Some(segment.reference().as_str()) {
                return None;
            }
            Some((
                obj.get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                obj.get("target")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ))
        })
        .collect();
    let context_source = context_segments
        .iter()
        .filter_map(|item| item.get("source").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let relevant_terms = crate::glossary::terms_in(
        all_terms,
        if context_source.is_empty() {
            &segment.source
        } else {
            &context_source
        },
    );
    let fixer = ReviewFixer::new(src, tgt, plan.clone(), config, "autofix.fix");
    let digest = chapter_digests
        .get(&location.0)
        .cloned()
        .unwrap_or_default();
    match fixer.propose(
        client,
        fixer_round,
        &segment.reference(),
        location.0,
        location.1,
        &segment.source,
        &segment.target,
        &verified,
        style,
        book_synopsis,
        &digest,
        &relevant_terms,
        &nearby_pairs,
    ) {
        Ok(patch) => {
            out.insert("status".to_string(), json!("planned"));
            out.insert(
                "patch".to_string(),
                json!({
                    "patch_id": patch.patch_id,
                    "round": patch.round,
                    "segment_ref": patch.segment_ref,
                    "chapter": patch.chapter,
                    "index": patch.index,
                    "before_hash": patch.before_hash,
                    "before": patch.before,
                    "after": patch.after,
                    "issue_ids": patch.issue_ids,
                    "status": "provisional",
                }),
            );
        }
        Err(error) => {
            out.insert("status".to_string(), json!("failed"));
            out.insert("reason".to_string(), json!(error.to_string()));
        }
    }
    out
}

/// Prepare candidates from shadow changes + re-verified issues (oracle
/// `AutofixCandidateService.prepare`). Serial; returns
/// `(records, overrides, issue_group_count)`.
#[allow(clippy::too_many_arguments)]
pub fn prepare_candidates(
    client: &mut dyn ModelClient,
    trace: &mut dyn ReviewTrace,
    chapters: &[ReviewChapter],
    all_terms: &[GlossaryTerm],
    analysis: &serde_json::Map<String, serde_json::Value>,
    changes: &[serde_json::Map<String, serde_json::Value>],
    issues: &[serde_json::Map<String, serde_json::Value>],
    style: &str,
    book_synopsis: &str,
    chapter_digests: &BTreeMap<i64, String>,
    src: &str,
    tgt: &str,
    plan: &crate::i18n::policy::PolicyPlan,
    config: &Config,
    fixer_round: i64,
    progress: &mut Progress<'_>,
) -> CandidateSet {
    let chapters_by_index: BTreeMap<i64, &ReviewChapter> = chapters
        .iter()
        .map(|chapter| (chapter.index, chapter))
        .collect();
    let mut records = Vec::new();
    let mut overrides: BTreeMap<(i64, i64), String> = BTreeMap::new();
    let mut ordered_changes = changes.to_vec();
    ordered_changes.sort_by(|a, b| {
        (
            a.get("chapter").and_then(|v| v.as_i64()).unwrap_or(-1),
            a.get("index").and_then(|v| v.as_i64()).unwrap_or(-1),
        )
            .cmp(&(
                b.get("chapter").and_then(|v| v.as_i64()).unwrap_or(-1),
                b.get("index").and_then(|v| v.as_i64()).unwrap_or(-1),
            ))
    });
    for change in &ordered_changes {
        let chapter_index = change.get("chapter").and_then(|v| v.as_i64());
        let text_index = change.get("index").and_then(|v| v.as_i64());
        let suggested = change.get("suggested_target").and_then(|v| v.as_str());
        let chapter = chapter_index.and_then(|index| chapters_by_index.get(&index));
        let valid = match (chapter, text_index, suggested) {
            (Some(chapter), Some(text_index), Some(suggested)) => {
                !suggested.trim().is_empty()
                    && text_index >= 0
                    && (text_index as usize) < chapter.segments.len()
            }
            _ => false,
        };
        if !valid {
            let keys: Vec<String> = change
                .get("issue_keys")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            add_record(
                &mut records,
                chapter_index.unwrap_or(-1),
                text_index.unwrap_or(-1),
                "",
                "change",
                "",
                None,
                &keys,
                &[],
                change
                    .get("review_result")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default(),
                "failed",
                "invalid_change",
                Some(change),
                &[],
                &[],
            );
            continue;
        }
        let (chapter_index, text_index, suggested) = (
            chapter_index.unwrap(),
            text_index.unwrap(),
            suggested.unwrap(),
        );
        let chapter = chapters_by_index[&chapter_index];
        let formal = chapter
            .segments
            .iter()
            .find(|(text, _, _, _)| *text == text_index)
            .map(|(_, _, _, target)| target.clone())
            .unwrap_or_default();
        let segment_index = chapter
            .segments
            .iter()
            .find(|(text, _, _, _)| *text == text_index)
            .map(|(_, segment, _, _)| *segment)
            .unwrap_or(0);
        let location = (chapter_index, text_index);
        let before = overrides.get(&location).cloned().unwrap_or(formal);
        let keys: Vec<String> = change
            .get("issue_keys")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let unchanged = suggested == before;
        add_record(
            &mut records,
            chapter_index,
            text_index,
            &format!("ch{chapter_index}:text{text_index}:seg{segment_index}"),
            "change",
            &before,
            Some(suggested),
            &keys,
            &[],
            change
                .get("review_result")
                .and_then(|v| v.as_str())
                .unwrap_or_default(),
            if unchanged {
                "not_applied_no_net_change"
            } else {
                "planned"
            },
            if unchanged { "unchanged_target" } else { "" },
            Some(change),
            &[],
            &[],
        );
        if !unchanged {
            overrides.insert(location, suggested.to_string());
        }
    }
    // Evidence over the change overlay for issue verification.
    let evidence_chapters: Vec<crate::review_evidence::EvidenceChapter> = chapters
        .iter()
        .map(|chapter| crate::review_evidence::EvidenceChapter {
            index: chapter.index,
            title: String::new(),
            digest: chapter_digests
                .get(&chapter.index)
                .cloned()
                .unwrap_or_default(),
            segments: chapter
                .segments
                .iter()
                .map(|(text_index, segment_index, source, target)| {
                    (*text_index, *segment_index, source.clone(), target.clone())
                })
                .collect(),
        })
        .collect();
    let mut evidence = BookEvidenceIndex::new(
        &evidence_chapters,
        all_terms.to_vec(),
        analysis.clone(),
        src,
        &overrides,
    );
    let mut grouped: BTreeMap<(i64, i64), Vec<serde_json::Map<String, serde_json::Value>>> =
        BTreeMap::new();
    for issue in issues {
        let chapter_index = issue.get("chapter").and_then(|v| v.as_i64());
        let text_index = issue.get("index").and_then(|v| v.as_i64());
        let in_range = match (chapter_index, text_index) {
            (Some(chapter_index), Some(text_index)) => chapters_by_index
                .get(&chapter_index)
                .is_some_and(|chapter| {
                    text_index >= 0 && (text_index as usize) < chapter.segments.len()
                }),
            _ => false,
        };
        if !in_range {
            let key = issue
                .get("issue_key")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let id = issue
                .get("issue_id")
                .and_then(|v| v.as_str())
                .unwrap_or(&key)
                .to_string();
            add_record(
                &mut records,
                chapter_index.unwrap_or(-1),
                text_index.unwrap_or(-1),
                "",
                "final_issue_fix",
                "",
                None,
                &[key],
                &[id],
                "",
                "failed",
                "invalid_issue_location",
                None,
                std::slice::from_ref(issue),
                &[],
            );
            continue;
        }
        let mut issue = issue.clone();
        if issue.get("issue_id").is_none() {
            if let Some(key) = issue.get("issue_key").cloned() {
                issue.insert("issue_id".to_string(), key);
            }
        }
        grouped
            .entry((chapter_index.unwrap(), text_index.unwrap()))
            .or_default()
            .push(issue);
    }
    let jobs: crate::review::IssueGroups = grouped.into_iter().collect();
    let group_count = jobs.len() as i64;
    if !jobs.is_empty() {
        progress.emit(0, jobs.len() as i64, "Final automatic revision");
    }
    for (position, (location, location_issues)) in jobs.iter().enumerate() {
        let fixed = fix_group(
            client,
            trace,
            &mut evidence,
            all_terms,
            style,
            book_synopsis,
            chapter_digests,
            src,
            tgt,
            plan,
            config,
            fixer_round,
            *location,
            location_issues,
        );
        progress.emit(
            position as i64 + 1,
            jobs.len() as i64,
            "Final automatic revision",
        );
        let current = evidence.segment_ref(location.0, location.1).cloned();
        let before = current
            .as_ref()
            .map(|segment| segment.target.clone())
            .unwrap_or_default();
        let reference = current
            .as_ref()
            .map(|segment| segment.reference())
            .unwrap_or_default();
        let agent_artifact = format!(
            "agents/r{fixer_round}-chunk-ch{}-base{}-n1.json",
            location.0, location.1
        );
        let fixer_artifact = format!("autofix/fixers/ch{}-text{}.json", location.0, location.1);
        let dismissed: Vec<serde_json::Map<String, serde_json::Value>> = fixed
            .get("dismissed")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_object().cloned())
                    .collect()
            })
            .unwrap_or_default();
        if !dismissed.is_empty() {
            let keys: Vec<String> = dismissed
                .iter()
                .filter_map(|issue| {
                    issue
                        .get("issue_key")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .collect();
            let ids: Vec<String> = dismissed
                .iter()
                .map(|issue| {
                    issue
                        .get("issue_id")
                        .or(issue.get("issue_key"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()
                })
                .collect();
            add_record(
                &mut records,
                location.0,
                location.1,
                &reference,
                "final_issue_review",
                &before,
                None,
                &keys,
                &ids,
                "",
                "dismissed",
                "dismissed_by_review_agent_loop",
                None,
                &dismissed,
                std::slice::from_ref(&agent_artifact),
            );
        }
        if fixed.get("status") == Some(&json!("dismissed")) {
            continue;
        }
        let verified: Vec<serde_json::Map<String, serde_json::Value>> = fixed
            .get("verified_issues")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_object().cloned())
                    .collect()
            })
            .unwrap_or_default();
        let patch = fixed.get("patch").and_then(|v| v.as_object()).cloned();
        match patch {
            Some(patch) => {
                let after = patch
                    .get("after")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let keys: Vec<String> = verified
                    .iter()
                    .filter_map(|issue| {
                        issue
                            .get("issue_key")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .collect();
                let ids: Vec<String> = verified
                    .iter()
                    .map(|issue| {
                        issue
                            .get("issue_id")
                            .or(issue.get("issue_key"))
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect();
                add_record(
                    &mut records,
                    location.0,
                    location.1,
                    &reference,
                    "final_issue_fix",
                    &before,
                    Some(after),
                    &keys,
                    &ids,
                    "confirmed_by_review_agent_loop",
                    "planned",
                    "",
                    None,
                    &verified,
                    &[agent_artifact, fixer_artifact],
                );
                overrides.insert(*location, after.to_string());
            }
            None => {
                let reason = fixed
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("fix_failed")
                    .to_string();
                let basis = if verified.is_empty() {
                    location_issues
                } else {
                    &verified
                };
                let keys: Vec<String> = basis
                    .iter()
                    .filter_map(|issue| {
                        issue
                            .get("issue_key")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .collect();
                let ids: Vec<String> = basis
                    .iter()
                    .map(|issue| {
                        issue
                            .get("issue_id")
                            .or(issue.get("issue_key"))
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect();
                add_record(
                    &mut records,
                    location.0,
                    location.1,
                    &reference,
                    "final_issue_fix",
                    &before,
                    None,
                    &keys,
                    &ids,
                    "",
                    "failed",
                    &reason,
                    None,
                    basis,
                    &[],
                );
            }
        }
    }
    (records, overrides, group_count)
}

/// Persist the plan + applying index before any formal write (oracle
/// `save_plan`).
#[allow(clippy::too_many_arguments)] // Mirrors the oracle save_plan shape.
pub fn save_plan(
    conn: &Connection,
    review_id: &str,
    round: i64,
    chapters: &[ReviewChapter],
    records: &[serde_json::Map<String, serde_json::Value>],
    overrides: &BTreeMap<(i64, i64), String>,
    inference: &BTreeMap<String, serde_json::Value>,
    reviewed_content_digest: &str,
    issue_group_count: i64,
    events: &mut Vec<serde_json::Value>,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let chapters_by_index: BTreeMap<i64, &ReviewChapter> = chapters
        .iter()
        .map(|chapter| (chapter.index, chapter))
        .collect();
    let mut locations = Vec::new();
    let mut sorted: Vec<((i64, i64), &String)> = overrides
        .iter()
        .map(|(location, target)| (*location, target))
        .collect();
    sorted.sort_by_key(|(location, _)| *location);
    for ((chapter_index, text_index), target) in sorted {
        let Some(chapter) = chapters_by_index.get(&chapter_index) else {
            continue;
        };
        let Some((_, segment_index, _, baseline)) = chapter
            .segments
            .iter()
            .find(|(text, _, _, _)| *text == text_index)
        else {
            continue;
        };
        let mut location = serde_json::Map::new();
        location.insert("chapter".to_string(), json!(chapter_index));
        location.insert("index".to_string(), json!(text_index));
        location.insert(
            "segment_ref".to_string(),
            json!(format!(
                "ch{chapter_index}:text{text_index}:seg{segment_index}"
            )),
        );
        location.insert("before".to_string(), json!(baseline));
        location.insert("before_hash".to_string(), json!(text_hash(baseline)));
        location.insert("target".to_string(), json!(target));
        location.insert("target_hash".to_string(), json!(text_hash(target)));
        location.insert(
            "record_ids".to_string(),
            json!(records
                .iter()
                .filter(|record| {
                    record.get("chapter").and_then(|v| v.as_i64()) == Some(chapter_index)
                        && record.get("index").and_then(|v| v.as_i64()) == Some(text_index)
                        && record.get("status").and_then(|v| v.as_str()) == Some("planned")
                })
                .filter_map(|record| record.get("record_id").cloned())
                .collect::<Vec<_>>()),
        );
        location.insert("status".to_string(), json!("pending"));
        location.insert("alignment_status".to_string(), json!("pending"));
        locations.push(serde_json::Value::Object(location));
    }
    let mut index = serde_json::Map::new();
    index.insert("version".to_string(), json!(1));
    index.insert(
        "inference".to_string(),
        serde_json::Value::Object(
            inference
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
    );
    index.insert("review_id".to_string(), json!(review_id));
    index.insert("status".to_string(), json!("applying"));
    index.insert(
        "reviewed_content_digest".to_string(),
        json!(reviewed_content_digest),
    );
    index.insert(
        "records".to_string(),
        json!(records
            .iter()
            .map(|record| json!(record))
            .collect::<Vec<_>>()),
    );
    index.insert("locations".to_string(), serde_json::Value::Array(locations));
    store::save_review_trace(
        conn,
        review_id,
        round,
        "autofix/index.json",
        &serde_json::Value::Object(index.clone()).to_string(),
    )?;
    events.push(json!({
        "event": "review_autofix_planned",
        "record_count": records.len() as i64,
        "location_count": index["locations"].as_array().map(|items| items.len() as i64).unwrap_or(0),
        "issue_group_count": issue_group_count,
    }));
    Ok(index)
}

/// Publish formal targets idempotently with optimistic checks (oracle
/// `AutofixPublisher.apply`). Annotation/style realignment is a no-op
/// until those port slices land; the status flow is preserved.
pub fn apply_index(
    conn: &Connection,
    book_id: &BookId,
    review_id: &str,
    round: i64,
    index: &mut serde_json::Map<String, serde_json::Value>,
    progress: &mut Progress<'_>,
) -> Result<()> {
    let locations: Vec<serde_json::Map<String, serde_json::Value>> = index
        .get("locations")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_object().cloned())
                .collect()
        })
        .unwrap_or_default();
    let mut by_chapter: BTreeMap<i64, Vec<serde_json::Map<String, serde_json::Value>>> =
        BTreeMap::new();
    for row in &locations {
        if let Some(chapter) = row.get("chapter").and_then(|v| v.as_i64()) {
            if !row.get("chapter").is_some_and(|v| v.is_boolean()) {
                by_chapter.entry(chapter).or_default().push(row.clone());
            }
        }
    }
    let total = locations.len();
    if total > 0 {
        progress.emit(0, total as i64, "Publishing Review Autofix");
    }
    let mut done = 0usize;
    let mut updated: Vec<serde_json::Map<String, serde_json::Value>> = Vec::new();
    for (chapter_index, mut rows) in by_chapter {
        rows.sort_by_key(|row| row.get("index").and_then(|v| v.as_i64()).unwrap_or(-1));
        let chapters = store::list_chapters(conn, book_id, 0, i64::MAX)?;
        let chapter_row = chapters
            .into_iter()
            .find(|chapter| chapter.ordinal == chapter_index);
        let segments = match &chapter_row {
            Some(chapter) => store::list_segments(conn, &chapter.id, 0, i64::MAX)?
                .into_iter()
                .filter(|segment| !segment.source.trim().is_empty())
                .collect::<Vec<_>>(),
            None => Vec::new(),
        };
        for mut row in rows {
            let text_index = row.get("index").and_then(|v| v.as_i64()).unwrap_or(-1);
            let target = row
                .get("target")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let before = row
                .get("before")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let valid = !row.get("index").is_some_and(|v| v.is_boolean())
                && text_index >= 0
                && (text_index as usize) < segments.len()
                && target.is_some()
                && before.is_some();
            if !valid {
                row.insert("status".to_string(), json!("failed"));
                row.insert("reason".to_string(), json!("invalid_index_location"));
                done += 1;
                updated.push(row);
                continue;
            }
            let (target, before) = (target.unwrap(), before.unwrap());
            let current = segments[text_index as usize]
                .target
                .clone()
                .unwrap_or_default();
            if current == target {
                row.insert(
                    "status".to_string(),
                    json!(if current == before {
                        "no_net_change"
                    } else {
                        "applied"
                    }),
                );
                if current != before && row.get("alignment_status") != Some(&json!("completed")) {
                    row.insert("alignment_status".to_string(), json!("completed"));
                }
            } else if current == before
                && text_hash(&current)
                    == row
                        .get("before_hash")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
            {
                store::set_target(
                    conn,
                    &segments[text_index as usize].id,
                    Some(&target),
                    "autofix",
                )?;
                row.insert("status".to_string(), json!("applied"));
                row.insert("alignment_status".to_string(), json!("completed"));
            } else {
                row.insert("status".to_string(), json!("failed"));
                row.insert("reason".to_string(), json!("formal_target_changed"));
                row.insert("actual_hash".to_string(), json!(text_hash(&current)));
            }
            done += 1;
            progress.emit(done as i64, total as i64, "Publishing Review Autofix");
            updated.push(row);
        }
        store::save_review_trace(
            conn,
            review_id,
            round,
            "autofix/index.json",
            &serde_json::Value::Object({
                let mut snapshot = index.clone();
                // Merge rows published so far (chapter-granular resume).
                let mut merged: Vec<serde_json::Value> = updated
                    .iter()
                    .map(|row| serde_json::Value::Object(row.clone()))
                    .collect();
                // Keep not-yet-visited rows from the previous snapshot.
                if let Some(previous) = snapshot.get("locations").and_then(|v| v.as_array()) {
                    for row in previous {
                        let key = (
                            row.get("chapter").and_then(|v| v.as_i64()),
                            row.get("index").and_then(|v| v.as_i64()),
                        );
                        if !updated.iter().any(|updated| {
                            (
                                updated.get("chapter").and_then(|v| v.as_i64()),
                                updated.get("index").and_then(|v| v.as_i64()),
                            ) == key
                        }) {
                            merged.push(row.clone());
                        }
                    }
                }
                merged.sort_by(|a, b| {
                    (
                        a.get("chapter").and_then(|v| v.as_i64()),
                        a.get("index").and_then(|v| v.as_i64()),
                    )
                        .cmp(&(
                            b.get("chapter").and_then(|v| v.as_i64()),
                            b.get("index").and_then(|v| v.as_i64()),
                        ))
                });
                snapshot.insert("locations".to_string(), serde_json::Value::Array(merged));
                snapshot
            })
            .to_string(),
        )?;
    }
    // Anything not visited (unknown chapter) keeps its pending row.
    let mut final_locations = updated;
    for row in locations {
        let key = (
            row.get("chapter").and_then(|v| v.as_i64()),
            row.get("index").and_then(|v| v.as_i64()),
        );
        if !final_locations.iter().any(|updated| {
            (
                updated.get("chapter").and_then(|v| v.as_i64()),
                updated.get("index").and_then(|v| v.as_i64()),
            ) == key
        }) {
            final_locations.push(row);
        }
    }
    final_locations.sort_by(|a, b| {
        (
            a.get("chapter").and_then(|v| v.as_i64()),
            a.get("index").and_then(|v| v.as_i64()),
        )
            .cmp(&(
                b.get("chapter").and_then(|v| v.as_i64()),
                b.get("index").and_then(|v| v.as_i64()),
            ))
    });
    index.insert(
        "locations".to_string(),
        json!(final_locations
            .iter()
            .map(|row| json!(row))
            .collect::<Vec<_>>()),
    );
    let location_status: BTreeMap<(Option<i64>, Option<i64>), String> = final_locations
        .iter()
        .map(|row| {
            (
                (
                    row.get("chapter").and_then(|v| v.as_i64()),
                    row.get("index").and_then(|v| v.as_i64()),
                ),
                row.get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    if let Some(records) = index.get_mut("records").and_then(|v| v.as_array_mut()) {
        for record in records {
            let Some(record) = record.as_object_mut() else {
                continue;
            };
            if record.get("status").and_then(|v| v.as_str()) != Some("planned") {
                continue;
            }
            match location_status
                .get(&(
                    record.get("chapter").and_then(|v| v.as_i64()),
                    record.get("index").and_then(|v| v.as_i64()),
                ))
                .map(String::as_str)
            {
                Some("applied") => {
                    record.insert("status".to_string(), json!("applied"));
                }
                Some("no_net_change") => {
                    record.insert("status".to_string(), json!("not_applied_no_net_change"));
                }
                Some("failed") => {
                    record.insert("status".to_string(), json!("not_applied"));
                }
                _ => {}
            }
        }
    }
    let has_failures = index
        .get("records")
        .and_then(|v| v.as_array())
        .is_some_and(|records| {
            records.iter().any(|record| {
                matches!(
                    record.get("status").and_then(|v| v.as_str()),
                    Some("failed" | "not_applied")
                )
            })
        })
        || final_locations
            .iter()
            .any(|row| row.get("status") == Some(&json!("failed")));
    index.insert(
        "status".to_string(),
        json!(if has_failures { "partial" } else { "completed" }),
    );
    store::save_review_trace(
        conn,
        review_id,
        round,
        "autofix/index.json",
        &serde_json::Value::Object(index.clone()).to_string(),
    )?;
    Ok(())
}

/// Merge the publication summary into the run (oracle `finish`).
pub fn finish_index(
    conn: &Connection,
    review_id: &str,
    index: &serde_json::Map<String, serde_json::Value>,
    events: &mut Vec<serde_json::Value>,
) -> Result<store::ReviewRun> {
    let records: Vec<serde_json::Map<String, serde_json::Value>> = index
        .get("records")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_object().cloned())
                .collect()
        })
        .unwrap_or_default();
    let locations: Vec<serde_json::Map<String, serde_json::Value>> = index
        .get("locations")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_object().cloned())
                .collect()
        })
        .unwrap_or_default();
    let applied = locations
        .iter()
        .filter(|row| row.get("status") == Some(&json!("applied")))
        .count();
    let failed_issue_records: Vec<&serde_json::Map<String, serde_json::Value>> = records
        .iter()
        .filter(|record| {
            record.get("origin") == Some(&json!("final_issue_fix"))
                && matches!(
                    record.get("status").and_then(|v| v.as_str()),
                    Some("failed" | "not_applied")
                )
        })
        .collect();
    let mut autofix = serde_json::Map::new();
    autofix.insert("enabled".to_string(), json!(true));
    autofix.insert(
        "status".to_string(),
        index.get("status").cloned().unwrap_or(json!("partial")),
    );
    autofix.insert("index".to_string(), json!("autofix/index.json"));
    autofix.insert("applied_segment_count".to_string(), json!(applied as i64));
    autofix.insert(
        "applied_change_count".to_string(),
        json!(records
            .iter()
            .filter(|record| {
                record.get("origin") == Some(&json!("change"))
                    && record.get("status") == Some(&json!("applied"))
            })
            .count() as i64),
    );
    autofix.insert(
        "applied_issue_fix_count".to_string(),
        json!(records
            .iter()
            .filter(|record| {
                record.get("origin") == Some(&json!("final_issue_fix"))
                    && record.get("status") == Some(&json!("applied"))
            })
            .count() as i64),
    );
    autofix.insert(
        "failed_issue_count".to_string(),
        json!(failed_issue_records
            .iter()
            .map(|record| {
                record
                    .get("issue_ids")
                    .and_then(|v| v.as_array())
                    .map(|ids| ids.len().max(1) as i64)
                    .unwrap_or(1)
            })
            .sum::<i64>()),
    );
    autofix.insert(
        "failed_record_count".to_string(),
        json!(records
            .iter()
            .filter(|record| matches!(
                record.get("status").and_then(|v| v.as_str()),
                Some("failed" | "not_applied")
            ))
            .count() as i64),
    );
    let run = store::load_review_run(conn, review_id)?.ok_or_else(|| crate::Error::NotFound {
        kind: "review",
        id: review_id.to_string(),
    })?;
    let mut summary: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&run.summary_json).unwrap_or_default();
    // Idempotent finish: same index + status already merged.
    if summary
        .get("autofix")
        .and_then(|v| v.as_object())
        .is_some_and(|existing| {
            existing.get("index") == Some(&json!("autofix/index.json"))
                && existing.get("status") == autofix.get("status")
        })
    {
        return Ok(run);
    }
    summary.insert(
        "autofix_applied_segment_count".to_string(),
        autofix
            .get("applied_segment_count")
            .cloned()
            .unwrap_or(json!(0)),
    );
    summary.insert(
        "autofix_failed_issue_count".to_string(),
        autofix
            .get("failed_issue_count")
            .cloned()
            .unwrap_or(json!(0)),
    );
    summary.insert(
        "autofix".to_string(),
        serde_json::Value::Object(autofix.clone()),
    );
    store::finish_review_run(
        conn,
        review_id,
        run.status.as_str(),
        run.termination.as_str(),
        &serde_json::Value::Object(summary).to_string(),
        run.issues_json.as_str(),
        run.changes_json.as_str(),
        None,
    )?;
    let mut event = serde_json::Map::new();
    for (key, value) in &autofix {
        event.insert(key.clone(), value.clone());
    }
    events.push({
        let mut payload = json!(event);
        payload["event"] = json!("review_autofix_finished");
        payload
    });
    store::append_review_event(
        conn,
        review_id,
        "review_autofix_finished",
        &serde_json::Value::Object(event).to_string(),
    )?;
    store::load_review_run(conn, review_id)?.ok_or_else(|| crate::Error::NotFound {
        kind: "review",
        id: review_id.to_string(),
    })
}

/// Run autofix end to end: resume-or-plan, apply, finish (oracle
/// `ReviewAutofixService.run`). `round` scopes the plan/index traces.
#[allow(clippy::too_many_arguments)]
pub fn run_autofix(
    client: &mut dyn ModelClient,
    conn: &Connection,
    project_id: &ProjectId,
    book_id: &BookId,
    review_id: &str,
    round: i64,
    all_terms: &[GlossaryTerm],
    analysis: &serde_json::Map<String, serde_json::Value>,
    style: &str,
    book_synopsis: &str,
    chapter_digests: &BTreeMap<i64, String>,
    src: &str,
    tgt: &str,
    plan: &crate::i18n::policy::PolicyPlan,
    config: &Config,
    progress: &mut Progress<'_>,
    events: &mut Vec<serde_json::Value>,
) -> Result<store::ReviewRun> {
    let _ = project_id;
    // Resume: an `applying` index re-applies; a finished index re-finishes.
    if let Some(text) = store::load_review_trace(conn, review_id, round, "autofix/index.json")? {
        let index: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        if let Some(index) = index.as_object() {
            match index.get("status").and_then(|v| v.as_str()) {
                Some("applying") => {
                    events.push(json!({"event": "review_autofix_resumed", "review_id": review_id}));
                    let mut index = index.clone();
                    apply_index(conn, book_id, review_id, round, &mut index, progress)?;
                    return finish_index(conn, review_id, &index, events);
                }
                Some("completed" | "partial") => {
                    return finish_index(conn, review_id, index, events);
                }
                _ => {}
            }
        }
    }
    let run = store::load_review_run(conn, review_id)?.ok_or_else(|| crate::Error::NotFound {
        kind: "review",
        id: review_id.to_string(),
    })?;
    let chapters = load_review_chapters(conn, book_id)?;
    let changes: Vec<serde_json::Map<String, serde_json::Value>> =
        serde_json::from_str::<serde_json::Value>(&run.changes_json)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_object().cloned())
                    .collect()
            })
            .unwrap_or_default();
    let issues: Vec<serde_json::Map<String, serde_json::Value>> =
        serde_json::from_str::<serde_json::Value>(&run.issues_json)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_object().cloned())
                    .collect()
            })
            .unwrap_or_default();
    let inference = prepare_identity(conn, review_id, round, &config.llm)?;
    let summary: serde_json::Value = serde_json::from_str(&run.summary_json).unwrap_or_default();
    let fixer_round = summary
        .get("round_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .max(0)
        + 1;
    let mut trace = TraceStore {
        conn,
        review_id: review_id.to_string(),
        round,
    };
    let (records, overrides, group_count) = prepare_candidates(
        client,
        &mut trace,
        &chapters,
        all_terms,
        analysis,
        &changes,
        &issues,
        style,
        book_synopsis,
        chapter_digests,
        src,
        tgt,
        plan,
        config,
        fixer_round,
        progress,
    );
    let mut index = save_plan(
        conn,
        review_id,
        round,
        &chapters,
        &records,
        &overrides,
        &inference,
        &run.reviewed_content_digest,
        group_count,
        events,
    )?;
    apply_index(conn, book_id, review_id, round, &mut index, progress)?;
    finish_index(conn, review_id, &index, events)
}
