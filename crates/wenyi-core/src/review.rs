//! Review orchestration: chunks, rounds, sessions.
//!
//! Oracle: `pipeline/review_chunks.py` + `review_rounds.py` +
//! `review_workflow.py` + `review_results.py` + `review_checkpoint.py`.
//! Serial execution (no thread pools): chunk scan with adaptive bisect,
//! verify loop, arbitration, fixer proposals, session state machine,
//! checkpoints, and result projections. Read-only: formal targets are
//! never written here (autofix owns publication).

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;
use serde_json::json;

use crate::config::Config;
use crate::glossary::GlossaryTerm;
use crate::model::{BookId, ProjectId};
use crate::pipeline::{Progress, RollingContext};
use crate::pipeline_agents::{ModelClient, ReviewFixer, ReviewOutputError, Reviewer};
use crate::review_conflicts::{
    apply_review_arbitrations, build_conflict_groups, normalize_review_issues,
    ReviewConflictArbiter,
};
use crate::review_evidence::{BookEvidenceIndex, EvidenceChapter};
use crate::review_loop::{MemoryTrace, ReviewAgentLoop, ReviewTrace};
use crate::review_models::{
    content_digest, review_overlay_digest, ReviewLoopOutcome, ReviewPolicy, ReviewRoundResult,
    ReviewSessionState,
};
use crate::{store, Result};

/// Pack paragraphs into contiguous blocks (oracle `pack_contiguous`).
pub fn pack_contiguous(lengths: &[(i64, usize)], budget: usize) -> Vec<Vec<i64>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut size = 0usize;
    for (text_index, tokens) in lengths {
        if !current.is_empty() && size + tokens > budget {
            chunks.push(std::mem::take(&mut current));
            size = 0;
        }
        current.push(*text_index);
        size += tokens;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Persisted trace adapter over `review_traces` (oracle `ReviewTraceStore`).
pub struct TraceStore<'a> {
    /// Database connection.
    pub conn: &'a Connection,
    /// Review identity.
    pub review_id: String,
    /// Active round (0 = unscoped).
    pub round: i64,
}

impl<'a> TraceStore<'a> {
    /// Sanitize an agent id into a trace key (oracle `_relative`).
    pub fn trace_key(agent_id: &str) -> String {
        let mut safe: String = agent_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        while safe.starts_with('-') {
            safe.remove(0);
        }
        while safe.ends_with('-') {
            safe.pop();
        }
        if safe.is_empty() {
            safe = "agent".to_string();
        }
        format!("agents/{safe}.json")
    }
}

impl ReviewTrace for TraceStore<'_> {
    fn load(&self, agent_id: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
        store::load_review_trace(
            self.conn,
            &self.review_id,
            self.round,
            &Self::trace_key(agent_id),
        )
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str(&text).ok())
    }

    fn save(&mut self, agent_id: &str, snapshot: &serde_json::Map<String, serde_json::Value>) {
        let _ = store::save_review_trace(
            self.conn,
            &self.review_id,
            self.round,
            &Self::trace_key(agent_id),
            &serde_json::Value::Object(snapshot.clone()).to_string(),
        );
    }

    fn log_event(&mut self, event: &str, data: serde_json::Map<String, serde_json::Value>) {
        let mut payload = data;
        payload
            .entry("review_round".to_string())
            .or_insert(json!(self.round));
        let _ = store::append_review_event(
            self.conn,
            &self.review_id,
            event,
            &serde_json::Value::Object(payload).to_string(),
        );
    }
}

/// One chapter's text segments for review (text_index, segment data).
pub struct ReviewChapter {
    /// Chapter index.
    pub index: i64,
    /// (text_index, segment_index, source, formal_target).
    pub segments: Vec<(i64, i64, String, String)>,
}

/// Review collaborators: config + client + agents.
pub struct ReviewRunner<'a> {
    /// Engine config.
    pub config: &'a Config,
    /// Model client.
    pub client: &'a mut dyn ModelClient,
    /// Source language.
    pub src: String,
    /// Target language.
    pub tgt: String,
}

impl<'a> ReviewRunner<'a> {
    /// Review one chapter's blocks serially (oracle `review_chapter`).
    #[allow(clippy::too_many_arguments)]
    pub fn review_chapter(
        &mut self,
        conn: &Connection,
        review_id: &str,
        round: i64,
        chapter: &ReviewChapter,
        terms: &[GlossaryTerm],
        evidence: &mut BookEvidenceIndex,
        target_overrides: &BTreeMap<(i64, i64), String>,
        review_round: Option<i64>,
        events: &mut Vec<serde_json::Value>,
    ) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
        let budget = (self.config.segment.max_tokens_per_batch.max(0) as usize) * 3;
        let lengths: Vec<(i64, usize)> = chapter
            .segments
            .iter()
            .map(|(text_index, _, source, _)| {
                (*text_index, crate::document::tokens::count_tokens(source))
            })
            .collect();
        let chunks = pack_contiguous(&lengths, budget.max(1));
        let mut base = 0usize;
        let mut jobs = Vec::new();
        for chunk in &chunks {
            jobs.push((base, chunk.clone()));
            base += chunk.len();
        }
        let round_prefix = review_round
            .map(|round| format!("r{round}-"))
            .unwrap_or_default();
        let mut out = Vec::new();
        for (chunk_base, chunk) in jobs {
            let issues = self.review_adaptive(
                conn,
                review_id,
                round,
                chapter,
                terms,
                evidence,
                target_overrides,
                review_round,
                &round_prefix,
                chunk_base as i64,
                &chunk,
                1,
                events,
            )?;
            out.extend(issues);
        }
        Ok(out)
    }

    /// Shrink malformed requests; bounded retries at singletons (oracle
    /// `review_adaptive`).
    #[allow(clippy::too_many_arguments)]
    fn review_adaptive(
        &mut self,
        conn: &Connection,
        review_id: &str,
        round: i64,
        chapter: &ReviewChapter,
        terms: &[GlossaryTerm],
        evidence: &mut BookEvidenceIndex,
        target_overrides: &BTreeMap<(i64, i64), String>,
        review_round: Option<i64>,
        round_prefix: &str,
        chunk_base: i64,
        chunk: &[i64],
        _attempt: i64,
        events: &mut Vec<serde_json::Value>,
    ) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
        match self.review_once(
            conn,
            review_id,
            round,
            chapter,
            terms,
            evidence,
            target_overrides,
            review_round,
            round_prefix,
            chunk_base,
            chunk,
            1,
            events,
        ) {
            Ok(issues) => Ok(issues),
            Err(reason) => {
                if chunk.len() > 1 {
                    let mid = chunk.len() / 2;
                    events.push(json!({
                        "event": "review_chunk_split",
                        "chapter": chapter.index,
                        "start_index": chunk_base,
                        "count": chunk.len(),
                        "left_count": mid,
                        "right_count": chunk.len() - mid,
                        "reason": reason,
                    }));
                    let mut left = self.review_adaptive(
                        conn,
                        review_id,
                        round,
                        chapter,
                        terms,
                        evidence,
                        target_overrides,
                        review_round,
                        round_prefix,
                        chunk_base,
                        &chunk[..mid],
                        1,
                        events,
                    )?;
                    let right = self.review_adaptive(
                        conn,
                        review_id,
                        round,
                        chapter,
                        terms,
                        evidence,
                        target_overrides,
                        review_round,
                        round_prefix,
                        chunk_base + mid as i64,
                        &chunk[mid..],
                        1,
                        events,
                    )?;
                    left.extend(right);
                    return Ok(left);
                }
                let retries = self.config.pipeline.review_output_retries.max(0);
                let mut last = reason;
                for attempt in 1..=retries {
                    events.push(json!({
                        "event": "review_singleton_retry",
                        "chapter": chapter.index,
                        "start_index": chunk_base,
                        "count": 1,
                        "attempt": attempt,
                        "max_retries": retries,
                        "reason": last,
                    }));
                    match self.review_once(
                        conn,
                        review_id,
                        round,
                        chapter,
                        terms,
                        evidence,
                        target_overrides,
                        review_round,
                        round_prefix,
                        chunk_base,
                        chunk,
                        attempt + 1,
                        events,
                    ) {
                        Ok(issues) => {
                            events.push(json!({
                                "event": "review_singleton_recovered",
                                "chapter": chapter.index,
                                "start_index": chunk_base,
                                "count": 1,
                                "attempt": attempt,
                            }));
                            return Ok(issues);
                        }
                        Err(reason) => last = reason,
                    }
                }
                events.push(json!({
                    "event": "review_singleton_failed",
                    "chapter": chapter.index,
                    "start_index": chunk_base,
                    "count": 1,
                    "attempts": retries + 1,
                    "reason": last,
                }));
                Err(crate::Error::Config(format!(
                    "Review output protocol error: {last}"
                )))
            }
        }
    }

    /// Run one review call; map block-local indices (oracle `review_once`).
    /// Returns issues or a protocol reason string.
    #[allow(clippy::too_many_arguments)]
    fn review_once(
        &mut self,
        conn: &Connection,
        review_id: &str,
        round: i64,
        chapter: &ReviewChapter,
        terms: &[GlossaryTerm],
        evidence: &mut BookEvidenceIndex,
        target_overrides: &BTreeMap<(i64, i64), String>,
        review_round: Option<i64>,
        round_prefix: &str,
        chunk_base: i64,
        chunk: &[i64],
        attempt: i64,
        events: &mut Vec<serde_json::Value>,
    ) -> std::result::Result<Vec<serde_json::Map<String, serde_json::Value>>, String> {
        let chunk_id = format!(
            "{round_prefix}ch{}-base{chunk_base}-n{}",
            chapter.index,
            chunk.len()
        );
        if let Ok(Some(cached)) = store::load_review_chunk(conn, review_id, &chunk_id) {
            let value: serde_json::Value = serde_json::from_str(&cached).unwrap_or_default();
            let issues = value
                .get("issues")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_object().cloned())
                        .collect()
                })
                .unwrap_or_default();
            return Ok(issues);
        }
        // Probe child caches with adaptive bisection boundaries.
        if chunk.len() > 1 {
            if let Some(merged) = Self::try_cached_subchunks(
                conn,
                review_id,
                round_prefix,
                chapter.index,
                chunk_base,
                chunk,
            ) {
                return Ok(merged);
            }
        }
        let positions: BTreeMap<i64, usize> = chapter
            .segments
            .iter()
            .enumerate()
            .map(|(position, (text_index, _, _, _))| (*text_index, position))
            .collect();
        let mut sources = Vec::new();
        let mut targets = Vec::new();
        for text_index in chunk {
            let Some(position) = positions.get(text_index) else {
                continue;
            };
            let (_, _, source, formal) = &chapter.segments[*position];
            sources.push(source.clone());
            targets.push(
                target_overrides
                    .get(&(chapter.index, *text_index))
                    .cloned()
                    .unwrap_or_else(|| formal.clone()),
            );
        }
        // Reuse a finished initial trace or run the reviewer.
        let initial_key = format!(
            "initial/initial-{round_prefix}ch{}-base{chunk_base}-n{}-attempt{attempt}.json",
            chapter.index,
            chunk.len()
        );
        let reused = store::load_review_trace(conn, review_id, round, &initial_key)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .filter(|value| {
                value.get("status") == Some(&json!("finished"))
                    && value.get("issues").is_some_and(|v| v.is_array())
            });
        let mut local: Vec<serde_json::Map<String, serde_json::Value>> = Vec::new();
        let initial_count: usize;
        if let Some(reused) = reused {
            if reused.get("json_repaired") == Some(&json!(true)) {
                events.push(json!({
                    "event": "review_json_repaired",
                    "chapter": chapter.index,
                    "start_index": chunk_base,
                    "count": chunk.len(),
                }));
            }
            for issue in reused
                .get("issues")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
            {
                // Reuse discards invalid cached indices (oracle: stale
                // traces must not turn the chunk into fallback).
                let index = issue.get("index").and_then(|v| v.as_i64()).unwrap_or(-1);
                if index >= 0 && (index as usize) < chunk.len() {
                    if let Some(obj) = issue.as_object() {
                        local.push(obj.clone());
                    }
                }
            }
            initial_count = reused
                .get("issues")
                .and_then(|v| v.as_array())
                .map(|items| items.len())
                .unwrap_or_default();
        } else {
            let plan = self.review_plan().map_err(|error| error.to_string())?;
            let reviewer = Reviewer::new(&self.src, &self.tgt, plan, self.config);
            let (found, repaired) = reviewer
                .review_result(self.client, &sources, &targets, terms)
                .map_err(|error: ReviewOutputError| error.reason.clone())?;
            if repaired {
                events.push(json!({
                    "event": "review_json_repaired",
                    "chapter": chapter.index,
                    "start_index": chunk_base,
                    "count": chunk.len(),
                }));
            }
            for issue in &found {
                if issue.index < 0 || (issue.index as usize) >= chunk.len() {
                    return Err("invalid_issue_index".to_string());
                }
                let mut obj = serde_json::Map::new();
                obj.insert("index".to_string(), json!(issue.index));
                obj.insert("type".to_string(), json!(issue.issue_type));
                obj.insert("detail".to_string(), json!(issue.detail));
                obj.insert("suggestion".to_string(), json!(issue.suggestion));
                local.push(obj);
            }
            initial_count = found.len();
            let mut trace = serde_json::Map::new();
            trace.insert("agent_id".to_string(), json!(initial_key));
            trace.insert("chapter".to_string(), json!(chapter.index));
            trace.insert("chunk_base".to_string(), json!(chunk_base));
            trace.insert("segment_count".to_string(), json!(chunk.len() as i64));
            trace.insert("attempt".to_string(), json!(attempt));
            trace.insert("status".to_string(), json!("finished"));
            trace.insert("json_repaired".to_string(), json!(repaired));
            trace.insert(
                "issues".to_string(),
                serde_json::Value::Array(local.iter().map(|i| json!(i)).collect()),
            );
            let _ = store::save_review_trace(
                conn,
                review_id,
                round,
                &initial_key,
                &serde_json::Value::Object(trace).to_string(),
            );
        }
        let before_agent = local.clone();
        let mut dismissed = Vec::new();
        let mut fallback_reason = String::new();
        if !local.is_empty() && self.config.pipeline.review_agent_loop {
            let plan = self.review_plan().map_err(|error| error.to_string())?;
            let mut agent = ReviewAgentLoop::new(
                &self.src,
                &self.tgt,
                plan,
                self.config,
                &mut *evidence,
                "review.verify",
            )
            .map_err(|error| error.to_string())?;
            let mut trace = TraceStore {
                conn,
                review_id: review_id.to_string(),
                round,
            };
            let outcome: ReviewLoopOutcome = agent.review_chunk(
                self.client,
                &mut trace,
                chapter.index,
                chunk_base,
                &sources,
                &targets,
                &local,
                review_round,
            );
            local = outcome.issues;
            dismissed = outcome.dismissed;
            fallback_reason = outcome.fallback_reason;
        }
        let mut mapped = Vec::new();
        for mut issue in local {
            let local_index = issue.get("index").and_then(|v| v.as_i64()).unwrap_or(-1);
            if local_index < 0 || (local_index as usize) >= chunk.len() {
                continue;
            }
            issue.insert("index".to_string(), json!(chunk_base + local_index));
            issue.insert("_chunk_id".to_string(), json!(chunk_id.clone()));
            if !fallback_reason.is_empty() {
                issue.insert(
                    "fallback_reason".to_string(),
                    json!(fallback_reason.clone()),
                );
            }
            mapped.push(issue);
        }
        events.push(json!({
            "event": "review_leaf_finished",
            "chapter": chapter.index,
            "chunk_base": chunk_base,
            "segment_count": chunk.len(),
            "initial_issue_count": initial_count as i64,
            "final_issue_count": mapped.len() as i64,
            "dismissed_count": dismissed.len() as i64,
            "fallback": !fallback_reason.is_empty(),
        }));
        let cached = json!({
            "issues": mapped,
            "initial_issues": before_agent,
            "dismissed": dismissed,
            "fallback_reason": fallback_reason,
        });
        let _ = store::save_review_chunk(conn, review_id, &chunk_id, &cached.to_string());
        Ok(mapped)
    }

    /// Probe child chunk caches recursively (oracle `try_cached_subchunks`).
    fn try_cached_subchunks(
        conn: &Connection,
        review_id: &str,
        round_prefix: &str,
        chapter_index: i64,
        chunk_base: i64,
        chunk: &[i64],
    ) -> Option<Vec<serde_json::Map<String, serde_json::Value>>> {
        fn probe(
            conn: &Connection,
            review_id: &str,
            round_prefix: &str,
            chapter_index: i64,
            base: i64,
            pieces: &[i64],
        ) -> Option<Vec<serde_json::Map<String, serde_json::Value>>> {
            if pieces.is_empty() {
                return Some(Vec::new());
            }
            let chunk_id = format!(
                "{round_prefix}ch{chapter_index}-base{base}-n{}",
                pieces.len()
            );
            if let Ok(Some(cached)) = store::load_review_chunk(conn, review_id, &chunk_id) {
                let value: serde_json::Value = serde_json::from_str(&cached).unwrap_or_default();
                return Some(
                    value
                        .get("issues")
                        .and_then(|v| v.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|v| v.as_object().cloned())
                                .collect()
                        })
                        .unwrap_or_default(),
                );
            }
            if pieces.len() <= 1 {
                return None;
            }
            let mid = pieces.len() / 2;
            let mut left = probe(
                conn,
                review_id,
                round_prefix,
                chapter_index,
                base,
                &pieces[..mid],
            )?;
            let right = probe(
                conn,
                review_id,
                round_prefix,
                chapter_index,
                base + mid as i64,
                &pieces[mid..],
            )?;
            left.extend(right);
            Some(left)
        }
        probe(
            conn,
            review_id,
            round_prefix,
            chapter_index,
            chunk_base,
            chunk,
        )
    }

    /// Frozen review plan for agents.
    fn review_plan(&self) -> Result<crate::i18n::policy::PolicyPlan> {
        let groups = crate::i18n::policy::task_groups_full(
            "review",
            self.config.pipeline.book_understanding,
            self.config.pipeline.polish,
            false,
            self.config.pipeline.review_agent_loop,
            self.config.pipeline.review_conflict_arbitration,
            self.config.pipeline.review_fix_loop,
            self.config.pipeline.review_autofix,
        )?;
        let refs: Vec<&str> = groups.iter().map(|s| s as &str).collect();
        crate::i18n::policy::resolve_policy(
            &self.src,
            &self.tgt,
            "review",
            &refs,
            &self.config.honorific_strategy,
        )
    }
}

/// Collapse shadow patches into final suggestions (oracle `net_changes`).
pub fn net_changes(
    baseline: &BTreeMap<(i64, i64), String>,
    overrides: &BTreeMap<(i64, i64), String>,
    patch_records: &[serde_json::Map<String, serde_json::Value>],
    active_patches: &BTreeMap<(i64, i64), serde_json::Map<String, serde_json::Value>>,
) -> Vec<serde_json::Map<String, serde_json::Value>> {
    let mut keys_by_location: BTreeMap<(i64, i64), BTreeSet<String>> = BTreeMap::new();
    for patch in patch_records {
        let chapter = patch.get("chapter").and_then(|v| v.as_i64());
        let index = patch.get("index").and_then(|v| v.as_i64());
        let (Some(chapter), Some(index)) = (chapter, index) else {
            continue;
        };
        if patch.get("status").and_then(|v| v.as_str()) == Some("rejected_cycle") {
            continue;
        }
        let keys = keys_by_location.entry((chapter, index)).or_default();
        if let Some(items) = patch.get("issue_keys").and_then(|v| v.as_array()) {
            for key in items.iter().filter_map(|v| v.as_str()) {
                if !key.is_empty() {
                    keys.insert(key.to_string());
                }
            }
        }
    }
    let mut changes = Vec::new();
    let mut locations: Vec<(i64, i64)> = overrides.keys().copied().collect();
    locations.sort();
    for location in locations {
        let suggested = &overrides[&location];
        if baseline.get(&location) == Some(suggested) {
            continue;
        }
        let status = active_patches
            .get(&location)
            .and_then(|patch| patch.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("provisional")
            .to_string();
        let mut change = serde_json::Map::new();
        change.insert("chapter".to_string(), json!(location.0));
        change.insert("index".to_string(), json!(location.1));
        change.insert("suggested_target".to_string(), json!(suggested));
        change.insert(
            "issue_keys".to_string(),
            json!(keys_by_location
                .get(&location)
                .map_or(Vec::new(), |keys| keys.iter().cloned().collect::<Vec<_>>())),
        );
        change.insert("review_result".to_string(), json!(status));
        changes.push(change);
    }
    changes
}

/// Remove internal fields for user-facing issues (oracle `project_issues`).
pub fn project_issues(
    issues: &[serde_json::Map<String, serde_json::Value>],
) -> Vec<serde_json::Map<String, serde_json::Value>> {
    let mut public: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for issue in issues {
        let key = issue
            .get("issue_key")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let chapter = issue.get("chapter").and_then(|v| v.as_i64());
        let index = issue.get("index").and_then(|v| v.as_i64());
        let (Some(chapter), Some(index)) = (chapter, index) else {
            continue;
        };
        if key.is_empty()
            || issue.get("chapter").is_some_and(|v| v.is_boolean())
            || issue.get("index").is_some_and(|v| v.is_boolean())
        {
            continue;
        }
        let mut projected = serde_json::Map::new();
        projected.insert("issue_key".to_string(), json!(key));
        projected.insert("chapter".to_string(), json!(chapter));
        projected.insert("index".to_string(), json!(index));
        projected.insert(
            "type".to_string(),
            json!(issue
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default()),
        );
        projected.insert(
            "detail".to_string(),
            json!(issue
                .get("detail")
                .and_then(|v| v.as_str())
                .unwrap_or_default()),
        );
        projected.insert(
            "suggestion".to_string(),
            json!(issue
                .get("suggestion")
                .and_then(|v| v.as_str())
                .unwrap_or_default()),
        );
        public.insert(key.to_string(), projected);
    }
    public.into_values().collect()
}

/// Review one whole-book round over an immutable shadow snapshot (oracle
/// `ReviewRoundService.review_round`).
#[allow(clippy::too_many_arguments)]
pub fn review_round(
    runner: &mut ReviewRunner,
    conn: &Connection,
    review_id: &str,
    round: i64,
    chapters: &[ReviewChapter],
    all_terms: &[GlossaryTerm],
    evidence: &mut BookEvidenceIndex,
    target_overrides: &BTreeMap<(i64, i64), String>,
    progress: &mut Progress<'_>,
    events: &mut Vec<serde_json::Value>,
) -> Result<ReviewRoundResult> {
    let total: i64 = chapters
        .iter()
        .map(|chapter| chapter.segments.len() as i64)
        .sum();
    let label = if round == 1 {
        format!("Whole-book review R{round}")
    } else {
        format!("Blind whole-book review R{round}")
    };
    progress.emit(0, total, &label);
    let mut done = 0i64;
    let mut raw_issues = Vec::new();
    for chapter in chapters {
        let mut chapter_issues = runner.review_chapter(
            conn,
            review_id,
            round,
            chapter,
            all_terms,
            evidence,
            target_overrides,
            Some(round),
            events,
        )?;
        for issue in &mut chapter_issues {
            issue.insert("chapter".to_string(), json!(chapter.index));
            issue.insert("stage".to_string(), json!("review_agent"));
            issue.insert("review_round".to_string(), json!(round));
        }
        done += chapter.segments.len() as i64;
        progress.emit(done, total, &label);
        events.push(json!({
            "event": "review_chapter_finished",
            "chapter": chapter.index,
            "segment_count": chapter.segments.len() as i64,
            "issue_count": chapter_issues.len() as i64,
        }));
        raw_issues.extend(chapter_issues);
    }
    let mut pre_arbitration = normalize_review_issues(&raw_issues, evidence);
    for issue in &mut pre_arbitration {
        let id = issue
            .get("issue_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        issue.insert("issue_id".to_string(), json!(format!("r{round}-{id}")));
    }
    let conflict_groups = build_conflict_groups(&pre_arbitration);
    let mut arbitrations = Vec::new();
    if !conflict_groups.is_empty() && runner.config.pipeline.review_conflict_arbitration {
        let total_groups = conflict_groups.len();
        for (position, group) in conflict_groups.iter().enumerate() {
            progress.emit(
                position as i64,
                total_groups as i64,
                &format!("Conflict arbitration R{round}"),
            );
            let plan = runner.review_plan()?;
            let mut arbiter = ReviewConflictArbiter::new(
                &runner.src.clone(),
                &runner.tgt.clone(),
                plan,
                runner.config,
                &mut *evidence,
            )?;
            let mut trace = TraceStore {
                conn,
                review_id: review_id.to_string(),
                round,
            };
            arbitrations.push(arbiter.arbitrate(runner.client, &mut trace, group));
            progress.emit(
                position as i64 + 1,
                total_groups as i64,
                &format!("Conflict arbitration R{round}"),
            );
        }
    } else if !conflict_groups.is_empty() {
        for group in &conflict_groups {
            let ids: Vec<serde_json::Value> = group
                .get("issues")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|issue| issue.get("issue_id").cloned())
                        .collect()
                })
                .unwrap_or_default();
            let mut arbitration = serde_json::Map::new();
            arbitration.insert(
                "conflict_id".to_string(),
                group.get("conflict_id").cloned().unwrap_or(json!("")),
            );
            arbitration.insert(
                "consistency_key".to_string(),
                group.get("consistency_key").cloned().unwrap_or(json!("")),
            );
            arbitration.insert(
                "issue_ids".to_string(),
                serde_json::Value::Array(ids.clone()),
            );
            arbitration.insert("status".to_string(), json!("unresolved"));
            arbitration.insert("recommended_value".to_string(), json!(""));
            arbitration.insert(
                "reason".to_string(),
                json!("Whole-book conflict arbitration is disabled in configuration."),
            );
            arbitration.insert(
                "supported_issue_ids".to_string(),
                serde_json::Value::Array(ids),
            );
            arbitration.insert("rejected_issue_ids".to_string(), json!([]));
            arbitration.insert("evidence_refs".to_string(), json!([]));
            arbitrations.push(arbitration);
        }
    }
    let (final_issues, superseded) = apply_review_arbitrations(&pre_arbitration, &arbitrations);
    let mut fallback_chunks = BTreeSet::new();
    for issue in &pre_arbitration {
        if issue.get("agent_fallback") == Some(&json!(true)) {
            if let Some(chunk) = issue.get("_chunk_id").and_then(|v| v.as_str()) {
                fallback_chunks.insert(chunk.to_string());
            }
        }
    }
    let residual_conflicts = build_conflict_groups(&final_issues);
    // Persist round artifacts for resume + auditing.
    let artifacts = [
        ("pre_arbitration_issues.json", serde_json::Value::Array(pre_arbitration.iter().map(|i| json!(i)).collect())),
        ("arbitration_superseded_issues.json", serde_json::Value::Array(superseded.iter().map(|i| json!(i)).collect())),
        ("final_issues.json", serde_json::Value::Array(final_issues.iter().map(|i| json!(i)).collect())),
        (
            "residual_conflicts.json",
            serde_json::Value::Array(
                residual_conflicts
                    .iter()
                    .map(|group| {
                        json!({
                            "conflict_id": group.get("conflict_id"),
                            "consistency_key": group.get("consistency_key"),
                            "issue_ids": group
                                .get("issues")
                                .and_then(|v| v.as_array())
                                .map(|items| {
                                    items.iter().filter_map(|issue| issue.get("issue_id").cloned()).collect::<Vec<_>>()
                                })
                                .unwrap_or_default(),
                        })
                    })
                    .collect(),
            ),
        ),
        (
            "conflicts.json",
            serde_json::Value::Array(
                conflict_groups
                    .iter()
                    .zip(arbitrations.iter())
                    .map(|(group, arbitration)| {
                        json!({
                            "conflict_id": group.get("conflict_id"),
                            "consistency_key": group.get("consistency_key"),
                            "status": arbitration.get("status"),
                        })
                    })
                    .collect(),
            ),
        ),
    ];
    for (key, value) in artifacts {
        let _ = store::save_review_trace(conn, review_id, round, key, &value.to_string());
    }
    events.push(json!({
        "event": "review_round_finished",
        "review_round": round,
        "issue_count": final_issues.len() as i64,
        "conflict_count": conflict_groups.len() as i64,
        "unresolved_conflict_count": residual_conflicts.len() as i64,
        "fallback_agent_count": fallback_chunks.len() as i64,
    }));
    Ok(ReviewRoundResult {
        issues: final_issues,
        pre_arbitration_issues: pre_arbitration,
        arbitration_superseded: superseded,
        conflict_groups,
        residual_conflicts,
        fallback_agent_count: fallback_chunks.len() as i64,
    })
}

/// Group confirmed issues by paragraph and propose replacements (oracle
/// `propose_review_patches`).
/// Patches + failures returned by patch proposal.
pub type PatchProposal = (
    Vec<serde_json::Map<String, serde_json::Value>>,
    Vec<serde_json::Map<String, serde_json::Value>>,
);
/// Issues grouped by (chapter, text index).
pub type IssueGroups = Vec<((i64, i64), Vec<serde_json::Map<String, serde_json::Value>>)>;
#[allow(clippy::too_many_arguments)]
pub fn propose_review_patches(
    runner: &mut ReviewRunner,
    conn: &Connection,
    review_id: &str,
    round: i64,
    round_result: &ReviewRoundResult,
    evidence: &mut BookEvidenceIndex,
    all_terms: &[GlossaryTerm],
    style: &str,
    book_synopsis: &str,
    progress: &mut Progress<'_>,
    events: &mut Vec<serde_json::Value>,
) -> Result<PatchProposal> {
    let mut grouped: BTreeMap<(i64, i64), Vec<serde_json::Map<String, serde_json::Value>>> =
        BTreeMap::new();
    let mut skipped = Vec::new();
    for issue in &round_result.issues {
        let chapter = issue.get("chapter").and_then(|v| v.as_i64());
        let index = issue.get("index").and_then(|v| v.as_i64());
        let issue_id = issue
            .get("issue_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let (Some(chapter), Some(index), Some(issue_id)) = (chapter, index, issue_id) else {
            let mut skip = serde_json::Map::new();
            skip.insert(
                "issue_id".to_string(),
                issue.get("issue_id").cloned().unwrap_or(json!(null)),
            );
            skip.insert("status".to_string(), json!("skipped"));
            skip.insert("reason".to_string(), json!("invalid_issue_location"));
            skipped.push(skip);
            continue;
        };
        if issue.get("chapter").is_some_and(|v| v.is_boolean())
            || issue.get("index").is_some_and(|v| v.is_boolean())
        {
            let mut skip = serde_json::Map::new();
            skip.insert("issue_id".to_string(), json!(issue_id));
            skip.insert("status".to_string(), json!("skipped"));
            skip.insert("reason".to_string(), json!("invalid_issue_location"));
            skipped.push(skip);
            continue;
        }
        if issue
            .get("arbitration")
            .and_then(|v| v.as_object())
            .is_some_and(|obj| obj.get("status") == Some(&json!("unresolved")))
        {
            let mut skip = serde_json::Map::new();
            skip.insert("issue_id".to_string(), json!(issue_id));
            skip.insert("chapter".to_string(), json!(chapter));
            skip.insert("index".to_string(), json!(index));
            skip.insert("status".to_string(), json!("skipped"));
            skip.insert(
                "reason".to_string(),
                json!("unresolved_consistency_conflict"),
            );
            skipped.push(skip);
            continue;
        }
        if runner.config.pipeline.review_agent_loop
            && issue.get("agent_fallback") == Some(&json!(true))
        {
            let mut skip = serde_json::Map::new();
            skip.insert("issue_id".to_string(), json!(issue_id));
            skip.insert("chapter".to_string(), json!(chapter));
            skip.insert("index".to_string(), json!(index));
            skip.insert("status".to_string(), json!("skipped"));
            skip.insert("reason".to_string(), json!("unverified_agent_fallback"));
            skipped.push(skip);
            continue;
        }
        grouped
            .entry((chapter, index))
            .or_default()
            .push(issue.clone());
    }
    let jobs: IssueGroups = grouped.into_iter().collect();
    if jobs.is_empty() {
        return Ok((Vec::new(), skipped));
    }
    let fix_round = round;
    progress.emit(
        0,
        jobs.len() as i64,
        &format!("Shadow revision R{fix_round}"),
    );
    let mut patches = Vec::new();
    let mut failures = Vec::new();
    for (position, ((chapter, index), issues)) in jobs.iter().enumerate() {
        let Some(segment) = evidence.segment_ref(*chapter, *index).cloned() else {
            let mut skip = serde_json::Map::new();
            skip.insert(
                "issue_ids".to_string(),
                json!(issues
                    .iter()
                    .filter_map(|issue| issue.get("issue_id").cloned())
                    .collect::<Vec<_>>()),
            );
            skip.insert("chapter".to_string(), json!(chapter));
            skip.insert("index".to_string(), json!(index));
            skip.insert("status".to_string(), json!("skipped"));
            skip.insert("reason".to_string(), json!("segment_not_found"));
            failures.push(skip);
            continue;
        };
        let context_args = serde_json::json!({
            "chapter": chapter, "index": index, "before": 4, "after": 4,
        });
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
        // Fixer trace reuse: identical round/ref/before/issues/after.
        let trace_key = format!("fixers/ch{chapter}-text{index}.json");
        let issue_ids: BTreeSet<String> = issues
            .iter()
            .filter_map(|issue| {
                issue
                    .get("issue_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();
        let cached = store::load_review_trace(conn, review_id, round, &trace_key)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .filter(|value| value.get("status") == Some(&json!("finished")))
            .and_then(|value| value.get("patch").and_then(|v| v.as_object()).cloned());
        let mut reused = None;
        if let Some(cached) = cached {
            let cached_ids: BTreeSet<String> = cached
                .get("issue_ids")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let after = cached
                .get("after")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if cached.get("round").and_then(|v| v.as_i64()) == Some(round)
                && cached.get("segment_ref").and_then(|v| v.as_str())
                    == Some(segment.reference().as_str())
                && cached.get("chapter").and_then(|v| v.as_i64()) == Some(*chapter)
                && cached.get("index").and_then(|v| v.as_i64()) == Some(*index)
                && cached.get("before").and_then(|v| v.as_str()) == Some(segment.target.as_str())
                && cached.get("before_hash").and_then(|v| v.as_str())
                    == Some(ReviewFixer::target_hash(&segment.target).as_str())
                && cached_ids == issue_ids
                && !after.trim().is_empty()
                && cached.get("status").and_then(|v| v.as_str()) == Some("provisional")
            {
                reused = Some(cached);
            }
        }
        if let Some(cached) = reused {
            patches.push(cached);
            progress.emit(
                position as i64 + 1,
                jobs.len() as i64,
                &format!("Shadow revision R{fix_round}"),
            );
            continue;
        }
        let mut trace = serde_json::Map::new();
        trace.insert("chapter".to_string(), json!(chapter));
        trace.insert("index".to_string(), json!(index));
        trace.insert("segment_ref".to_string(), json!(segment.reference()));
        trace.insert(
            "issue_ids".to_string(),
            json!(issue_ids.iter().cloned().collect::<Vec<_>>()),
        );
        trace.insert("status".to_string(), json!("running"));
        let _ = store::save_review_trace(
            conn,
            review_id,
            round,
            &trace_key,
            &serde_json::Value::Object(trace.clone()).to_string(),
        );
        let plan = runner.review_plan()?;
        let fixer = ReviewFixer::new(
            &runner.src.clone(),
            &runner.tgt.clone(),
            plan,
            runner.config,
            "review.fix",
        );
        let digest = evidence
            .chapter_digests
            .get(chapter)
            .cloned()
            .unwrap_or_default();
        match fixer.propose(
            runner.client,
            round,
            &segment.reference(),
            *chapter,
            *index,
            &segment.source,
            &segment.target,
            issues,
            style,
            book_synopsis,
            &digest,
            &relevant_terms,
            &nearby_pairs,
        ) {
            Ok(patch) => {
                let mut saved = serde_json::Map::new();
                saved.insert("patch_id".to_string(), json!(patch.patch_id));
                saved.insert("round".to_string(), json!(patch.round));
                saved.insert("segment_ref".to_string(), json!(patch.segment_ref));
                saved.insert("chapter".to_string(), json!(patch.chapter));
                saved.insert("index".to_string(), json!(patch.index));
                saved.insert("before_hash".to_string(), json!(patch.before_hash));
                saved.insert("before".to_string(), json!(patch.before));
                saved.insert("after".to_string(), json!(patch.after));
                saved.insert("issue_ids".to_string(), json!(patch.issue_ids));
                saved.insert("status".to_string(), json!("provisional"));
                trace.insert("status".to_string(), json!("finished"));
                trace.insert(
                    "patch".to_string(),
                    serde_json::Value::Object(saved.clone()),
                );
                let _ = store::save_review_trace(
                    conn,
                    review_id,
                    round,
                    &trace_key,
                    &serde_json::Value::Object(trace).to_string(),
                );
                patches.push(saved);
            }
            Err(error) => {
                trace.insert("status".to_string(), json!("failed"));
                trace.insert(
                    "error".to_string(),
                    json!({"type": "ReviewFixerProtocolError", "message": error.reason}),
                );
                let _ = store::save_review_trace(
                    conn,
                    review_id,
                    round,
                    &trace_key,
                    &serde_json::Value::Object(trace).to_string(),
                );
                let mut failure = serde_json::Map::new();
                failure.insert(
                    "issue_ids".to_string(),
                    json!(issue_ids.iter().cloned().collect::<Vec<_>>()),
                );
                failure.insert("chapter".to_string(), json!(chapter));
                failure.insert("index".to_string(), json!(index));
                failure.insert("segment_ref".to_string(), json!(segment.reference()));
                failure.insert("status".to_string(), json!("failed"));
                failure.insert("reason".to_string(), json!(error.to_string()));
                failures.push(failure);
            }
        }
        progress.emit(
            position as i64 + 1,
            jobs.len() as i64,
            &format!("Shadow revision R{fix_round}"),
        );
    }
    events.push(json!({
        "event": "review_fix_round_finished",
        "review_round": round,
        "patch_count": patches.len() as i64,
        "skipped_count": skipped.len() as i64,
        "failed_count": failures.len() as i64,
    }));
    failures.extend(skipped.into_iter().map(|mut skip| {
        // Skips surface as failures for prepare_fix blocking (oracle merges
        // skipped into failures via the failures list).
        if skip.get("status").and_then(|v| v.as_str()) == Some("skipped") {
            skip.insert("status".to_string(), json!("skipped"));
        }
        skip
    }));
    Ok((patches, failures))
}

/// Save the session checkpoint (oracle `ReviewCheckpoint.save`).
pub fn save_checkpoint(
    conn: &Connection,
    review_id: &str,
    state: &ReviewSessionState,
    current_round: i64,
    phase: &str,
    latest: Option<&ReviewRoundResult>,
) -> Result<()> {
    let mut payload = serde_json::Map::new();
    payload.insert("phase".to_string(), json!(phase));
    payload.insert(
        "next_round".to_string(),
        json!(if phase == "round_done" {
            current_round + 1
        } else {
            current_round
        }),
    );
    let mut overrides: Vec<serde_json::Value> = state
        .target_overrides
        .iter()
        .map(|((chapter, index), target)| {
            json!({"chapter": chapter, "index": index, "target": target})
        })
        .collect();
    overrides.sort_by(|a, b| {
        (a["chapter"].as_i64(), a["index"].as_i64())
            .cmp(&(b["chapter"].as_i64(), b["index"].as_i64()))
    });
    payload.insert(
        "target_overrides".to_string(),
        serde_json::Value::Array(overrides),
    );
    payload.insert(
        "seen_overlays".to_string(),
        json!(state.seen_overlays.iter().cloned().collect::<Vec<_>>()),
    );
    payload.insert(
        "patch_records".to_string(),
        json!(state
            .patch_records
            .iter()
            .map(|p| json!(p))
            .collect::<Vec<_>>()),
    );
    let mut active: Vec<serde_json::Value> = state
        .active_patches
        .iter()
        .map(|((chapter, index), patch)| {
            let mut merged = patch.clone();
            merged.insert("chapter".to_string(), json!(chapter));
            merged.insert("index".to_string(), json!(index));
            json!(merged)
        })
        .collect();
    active.sort_by(|a, b| {
        (a["chapter"].as_i64(), a["index"].as_i64())
            .cmp(&(b["chapter"].as_i64(), b["index"].as_i64()))
    });
    payload.insert(
        "active_patches".to_string(),
        serde_json::Value::Array(active),
    );
    payload.insert(
        "fix_failures".to_string(),
        json!(state
            .fix_failures
            .iter()
            .map(|f| json!(f))
            .collect::<Vec<_>>()),
    );
    payload.insert("blocked_issues".to_string(), json!(state.blocked_issues));
    payload.insert(
        "round_summaries".to_string(),
        json!(state
            .round_summaries
            .iter()
            .map(|s| json!(s))
            .collect::<Vec<_>>()),
    );
    payload.insert("clean_streak".to_string(), json!(state.clean_streak));
    payload.insert("fix_rounds".to_string(), json!(state.fix_rounds));
    if let Some(latest) = latest {
        payload.insert(
            "latest_issues".to_string(),
            json!(latest.issues.iter().map(|i| json!(i)).collect::<Vec<_>>()),
        );
        payload.insert(
            "latest_pre_arbitration_issues".to_string(),
            json!(latest
                .pre_arbitration_issues
                .iter()
                .map(|i| json!(i))
                .collect::<Vec<_>>()),
        );
        payload.insert(
            "latest_arbitration_superseded".to_string(),
            json!(latest
                .arbitration_superseded
                .iter()
                .map(|i| json!(i))
                .collect::<Vec<_>>()),
        );
        payload.insert(
            "latest_conflict_groups".to_string(),
            json!(latest
                .conflict_groups
                .iter()
                .map(|g| json!(g))
                .collect::<Vec<_>>()),
        );
        payload.insert(
            "latest_residual_conflicts".to_string(),
            json!(latest
                .residual_conflicts
                .iter()
                .map(|g| json!(g))
                .collect::<Vec<_>>()),
        );
        payload.insert(
            "latest_fallback_agent_count".to_string(),
            json!(latest.fallback_agent_count),
        );
    }
    store::save_review_checkpoint(
        conn,
        review_id,
        &serde_json::Value::Object(payload).to_string(),
    )
}

/// Restore a checkpoint into session state (oracle `ReviewCheckpoint.restore`
/// decode half). Returns `(next_round, scan_done_latest)`.
pub fn restore_checkpoint(
    checkpoint_json: &str,
    initial_digest: &str,
    max_review_rounds: i64,
) -> (ReviewSessionState, i64, Option<ReviewRoundResult>) {
    let checkpoint: serde_json::Value = serde_json::from_str(checkpoint_json).unwrap_or_default();
    let mut state = ReviewSessionState::default();
    state.seen_overlays.insert(initial_digest.to_string());
    let Some(checkpoint) = checkpoint.as_object() else {
        return (state, 1, None);
    };
    let mut start_round = checkpoint
        .get("next_round")
        .and_then(|v| v.as_i64())
        .unwrap_or(1)
        .min(max_review_rounds);
    for entry in checkpoint
        .get("target_overrides")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        if let (Some(chapter), Some(index), Some(target)) = (
            entry.get("chapter").and_then(|v| v.as_i64()),
            entry.get("index").and_then(|v| v.as_i64()),
            entry.get("target").and_then(|v| v.as_str()),
        ) {
            state
                .target_overrides
                .insert((chapter, index), target.to_string());
        }
    }
    state.seen_overlays = checkpoint
        .get("seen_overlays")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    state.patch_records = checkpoint
        .get("patch_records")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_object().cloned())
                .collect()
        })
        .unwrap_or_default();
    let history_by_id: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = state
        .patch_records
        .iter()
        .filter_map(|patch| {
            patch
                .get("patch_id")
                .and_then(|v| v.as_str())
                .map(|id| (id.to_string(), patch.clone()))
        })
        .collect();
    state.active_patches = checkpoint
        .get("active_patches")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|entry| {
                    let entry = entry.as_object()?;
                    let chapter = entry.get("chapter")?.as_i64()?;
                    let index = entry.get("index")?.as_i64()?;
                    let resolved = entry
                        .get("patch_id")
                        .and_then(|v| v.as_str())
                        .and_then(|id| history_by_id.get(id))
                        .cloned()
                        .unwrap_or_else(|| entry.clone());
                    Some(((chapter, index), resolved))
                })
                .collect()
        })
        .unwrap_or_default();
    state.fix_failures = checkpoint
        .get("fix_failures")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_object().cloned())
                .collect()
        })
        .unwrap_or_default();
    state.blocked_issues = checkpoint
        .get("blocked_issues")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(key, value)| {
                    value.as_object().map(|issue| (key.clone(), issue.clone()))
                })
                .collect()
        })
        .unwrap_or_default();
    state.round_summaries = checkpoint
        .get("round_summaries")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_object().cloned())
                .collect()
        })
        .unwrap_or_default();
    state.clean_streak = checkpoint
        .get("clean_streak")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    state.fix_rounds = checkpoint
        .get("fix_rounds")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let mut resume_latest = None;
    if checkpoint.get("phase").and_then(|v| v.as_str()) == Some("scan_done") {
        let scan_round = checkpoint
            .get("next_round")
            .and_then(|v| v.as_i64())
            .unwrap_or(1);
        if scan_round > max_review_rounds {
            start_round = max_review_rounds;
        } else {
            start_round = scan_round;
            let issues = |key: &str| {
                checkpoint
                    .get(key)
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| v.as_object().cloned())
                            .collect()
                    })
                    .unwrap_or_default()
            };
            resume_latest = Some(ReviewRoundResult {
                issues: issues("latest_issues"),
                pre_arbitration_issues: issues("latest_pre_arbitration_issues"),
                arbitration_superseded: issues("latest_arbitration_superseded"),
                conflict_groups: issues("latest_conflict_groups"),
                residual_conflicts: issues("latest_residual_conflicts"),
                fallback_agent_count: checkpoint
                    .get("latest_fallback_agent_count")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
            });
        }
    }
    (state, start_round, resume_latest)
}

/// Run one read-only whole-book session (oracle `run_session`).
#[allow(clippy::too_many_arguments)]
pub fn run_session(
    runner: &mut ReviewRunner,
    conn: &Connection,
    _project_id: &ProjectId,
    _book_id: &BookId,
    review_id: &str,
    chapters: &[ReviewChapter],
    all_terms: &[GlossaryTerm],
    analysis: &serde_json::Map<String, serde_json::Value>,
    digests: &BTreeMap<i64, String>,
    style: &str,
    book_synopsis: &str,
    progress: &mut Progress<'_>,
    events: &mut Vec<serde_json::Value>,
) -> Result<store::ReviewRun> {
    let policy = ReviewPolicy {
        fix_loop: runner.config.pipeline.review_fix_loop,
        max_fix_rounds: runner.config.pipeline.review_fix_max_rounds,
        clean_confirmations: runner.config.pipeline.review_clean_confirmations,
    };
    let max_rounds = policy.max_review_rounds();
    let formal: Vec<(i64, Vec<String>)> = chapters
        .iter()
        .map(|chapter| {
            (
                chapter.index,
                chapter
                    .segments
                    .iter()
                    .map(|(_, _, _, target)| target.clone())
                    .collect(),
            )
        })
        .collect();
    let initial_digest = review_overlay_digest(&formal, &BTreeMap::new());
    let (mut state, start_round, mut resume_latest) =
        match store::load_review_checkpoint(conn, review_id)? {
            Some(text) => restore_checkpoint(&text, &initial_digest, max_rounds),
            None => {
                let mut state = ReviewSessionState::default();
                state.seen_overlays.insert(initial_digest.clone());
                (state, 1, None)
            }
        };
    if start_round > 1 || resume_latest.is_some() {
        events.push(json!({
            "event": "review_checkpoint_restored",
            "next_round": start_round,
            "phase": if resume_latest.is_some() { "scan_done" } else { "round_done" },
            "override_count": state.target_overrides.len() as i64,
            "fix_rounds": state.fix_rounds,
            "clean_streak": state.clean_streak,
        }));
    }
    let evidence_chapters: Vec<EvidenceChapter> = chapters
        .iter()
        .map(|chapter| EvidenceChapter {
            index: chapter.index,
            title: String::new(),
            digest: digests.get(&chapter.index).cloned().unwrap_or_default(),
            segments: chapter
                .segments
                .iter()
                .map(|(text_index, segment_index, source, target)| {
                    (*text_index, *segment_index, source.clone(), target.clone())
                })
                .collect(),
        })
        .collect();
    let overlay_digest = review_overlay_digest(&formal, &state.target_overrides);
    let mut round = start_round;
    loop {
        if round > max_rounds {
            state.termination = "max_rounds".to_string();
            break;
        }
        // Fresh shadow view per round over the immutable overrides.
        let mut evidence = BookEvidenceIndex::new(
            &evidence_chapters,
            all_terms.to_vec(),
            analysis.clone(),
            &runner.src.clone(),
            &state.target_overrides,
        );
        events.push(json!({
            "event": "review_round_started",
            "review_round": round,
        }));
        let latest = match resume_latest.take() {
            Some(latest) => {
                events.push(json!({
                    "event": "review_scan_skipped",
                    "review_round": round,
                }));
                latest
            }
            None => review_round(
                runner,
                conn,
                review_id,
                round,
                chapters,
                all_terms,
                &mut evidence,
                &state.target_overrides,
                progress,
                events,
            )?,
        };
        state.latest = Some(latest.clone());
        let decision = state.after_scan(&latest, round, &overlay_digest, policy);
        state.round_summaries.push(decision.summary.clone());
        events.push(json!({
            "event": "review_round_finished",
            "review_round": round,
            "action": decision.action,
        }));
        // Checkpoint the completed scan (resume skips model calls).
        save_checkpoint(conn, review_id, &state, round, "scan_done", Some(&latest))?;
        if decision.action != "fix" {
            save_checkpoint(conn, review_id, &state, round, "round_done", Some(&latest))?;
            if decision.action == "stop" {
                break;
            }
            round += 1;
            continue;
        }
        let (patches, failures) = propose_review_patches(
            runner,
            conn,
            review_id,
            round,
            &latest,
            &mut evidence,
            all_terms,
            style,
            book_synopsis,
            progress,
            events,
        )?;
        let mut current_targets: BTreeMap<(i64, i64), String> = BTreeMap::new();
        for chapter in chapters {
            for (text_index, _, _, formal_target) in &chapter.segments {
                let location = (chapter.index, *text_index);
                current_targets.insert(
                    location,
                    state
                        .target_overrides
                        .get(&location)
                        .cloned()
                        .unwrap_or_else(|| formal_target.clone()),
                );
            }
        }
        let mut summary = decision.summary.clone();
        let Some(plan) = state.prepare_fix(
            &latest,
            patches,
            failures,
            &current_targets,
            &formal,
            round,
            &mut summary,
        ) else {
            state.round_summaries.push(summary);
            save_checkpoint(conn, review_id, &state, round, "round_done", Some(&latest))?;
            break;
        };
        if !state.after_fix(&plan, &overlay_digest, &mut summary) {
            state.round_summaries.push(summary);
            save_checkpoint(conn, review_id, &state, round, "round_done", Some(&latest))?;
            break;
        }
        state.round_summaries.push(summary);
        save_checkpoint(conn, review_id, &state, round, "round_done", Some(&latest))?;
        round += 1;
        // Recompute the overlay digest for the next round's guards.
        let _ = review_overlay_digest(&formal, &state.target_overrides);
    }
    // Project results (read-only; formal text untouched).
    let baseline: BTreeMap<(i64, i64), String> = chapters
        .iter()
        .flat_map(|chapter| {
            chapter
                .segments
                .iter()
                .map(|(text_index, _, _, target)| ((chapter.index, *text_index), target.clone()))
        })
        .collect();
    let changes = net_changes(
        &baseline,
        &state.target_overrides,
        &state.patch_records,
        &state.active_patches,
    );
    let issues = match &state.latest {
        Some(latest) => project_issues(&state.effective_issues(latest)),
        None => Vec::new(),
    };
    let summary = json!({
        "issue_count": issues.len() as i64,
        "change_count": changes.len() as i64,
        "round_count": state.round_summaries.len() as i64,
        "fix_rounds": state.fix_rounds,
    });
    let status = if state.termination == "clean_confirmed" || state.termination == "issues_reported"
    {
        "completed"
    } else {
        "partial"
    };
    store::finish_review_run(
        conn,
        review_id,
        status,
        &state.termination,
        &summary.to_string(),
        &serde_json::Value::Array(issues.into_iter().map(serde_json::Value::Object).collect())
            .to_string(),
        &serde_json::Value::Array(changes.into_iter().map(serde_json::Value::Object).collect())
            .to_string(),
        None,
    )?;
    events.push(json!({
        "event": "review_finished",
        "review_id": review_id,
        "status": status,
        "termination": state.termination,
    }));
    store::load_review_run(conn, review_id)?.ok_or_else(|| crate::Error::NotFound {
        kind: "review",
        id: review_id.to_string(),
    })
}

/// Load reviewable chapters from the store (empty-source segments dropped).
pub fn load_review_chapters(conn: &Connection, book_id: &BookId) -> Result<Vec<ReviewChapter>> {
    let mut out = Vec::new();
    for chapter in store::list_chapters(conn, book_id, 0, i64::MAX)? {
        let mut segments = Vec::new();
        let mut text_index = 0i64;
        for segment in store::list_segments(conn, &chapter.id, 0, i64::MAX)? {
            if segment.source.trim().is_empty() {
                continue;
            }
            segments.push((
                text_index,
                segment.ordinal,
                segment.source,
                segment.target.unwrap_or_default(),
            ));
            text_index += 1;
        }
        out.push(ReviewChapter {
            index: chapter.ordinal,
            segments,
        });
    }
    Ok(out)
}

/// Gate: every chapter must be `done` (oracle `_open_session` gate).
pub fn review_gate(conn: &Connection, book_id: &BookId) -> Result<Vec<i64>> {
    store::pending_chapters(conn, book_id)
}

/// Formal content digest for resume matching (oracle `content_digest`).
pub fn formal_content_digest(conn: &Connection, book_id: &BookId) -> Result<String> {
    let mut chapters = Vec::new();
    for chapter in store::list_chapters(conn, book_id, 0, i64::MAX)? {
        let mut segments = Vec::new();
        for segment in store::list_segments(conn, &chapter.id, 0, i64::MAX)? {
            if segment.source.trim().is_empty() {
                continue;
            }
            segments.push((
                segment.ordinal,
                segment.anchor,
                segment.kind,
                segment.source,
                segment.target.unwrap_or_default(),
            ));
        }
        chapters.push((chapter.ordinal, segments));
    }
    Ok(content_digest(&chapters))
}

/// Silence unused-import lints for session-only builds.
#[allow(dead_code)]
fn _session_hint(_: &RollingContext, _: &MemoryTrace) {}
