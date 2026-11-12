//! Review domain: pure outcomes, stable identities, shadow sessions.
//!
//! Oracle: `review/models.py` + `review/session.py`. Read-only whole-book
//! review state: stable issue keys, segment refs, overlay/content digests,
//! and the shadow-session state machine (`after_scan`/`prepare_fix`/
//! `after_fix`). No file or model I/O here.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

/// Consistency proposal kinds (oracle `CONSISTENCY_KINDS`).
pub const CONSISTENCY_KINDS: [&str; 3] = ["term", "pronoun", "fixed"];

/// Deterministic candidate ID shared by snapshots and the agent protocol.
pub fn review_candidate_id(
    chapter: i64,
    chunk_base: i64,
    ordinal: i64,
    review_round: Option<i64>,
) -> String {
    match review_round {
        Some(round) => format!("r{round}-ch{chapter}-base{chunk_base}-candidate{ordinal}"),
        None => format!("ch{chapter}-base{chunk_base}-candidate{ordinal}"),
    }
}

/// Accept strings only, stripped (oracle `clean_text`).
pub fn clean_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.trim().to_string(),
        _ => String::new(),
    }
}

/// NFKC + casefold + trim for comparing proposed values.
pub fn normalize_value(value: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    value
        .nfkc()
        .collect::<String>()
        .to_lowercase()
        .trim()
        .to_string()
}

/// UTF-8 SHA-256 hex for optimistic patch validation (oracle `text_hash`).
pub fn text_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    crate::document::hash::hex_of(&hasher.finalize())
}

/// Collapse whitespace after identity normalization.
fn identity_text(value: &serde_json::Value) -> String {
    let normalized = normalize_value(&clean_text(value));
    let mut out = String::with_capacity(normalized.len());
    let mut gap = false;
    for word in normalized.split_whitespace() {
        if gap {
            out.push(' ');
        }
        out.push_str(word);
        gap = true;
    }
    out
}

/// Stable issue key across rounds (oracle `review_issue_key`).
pub fn review_issue_key(issue: &serde_json::Map<String, serde_json::Value>) -> String {
    let consistency_key = issue
        .get("consistency")
        .and_then(|v| v.as_object())
        .map(|obj| obj.get("key").map(identity_text).unwrap_or_default())
        .unwrap_or_default();
    let subject = if consistency_key.is_empty() {
        issue.get("detail").map(identity_text).unwrap_or_default()
    } else {
        consistency_key
    };
    let payload = json!([
        issue
            .get("chapter")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        issue
            .get("index")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        issue.get("type").map(identity_text).unwrap_or_default(),
        subject,
    ]);
    format!("review-issue-{}", &text_hash(&payload.to_string())[..20])
}

/// One reviewable paragraph with its stable book position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRef {
    /// Global position across the book.
    pub global_ordinal: i64,
    /// Chapter index.
    pub chapter: i64,
    /// Text-segment position within the chapter.
    pub text_index: i64,
    /// Store segment ordinal.
    pub segment_index: i64,
    /// Source text.
    pub source: String,
    /// Effective (shadow-aware) target.
    pub target: String,
    /// Formal target before shadow overrides.
    pub baseline_target: String,
    /// `formal` or `shadow_override`.
    pub target_origin: String,
    /// Chapter title.
    pub chapter_title: String,
}

impl SegmentRef {
    /// Stable diagnostic ID (oracle `SegmentRef.ref`).
    pub fn reference(&self) -> String {
        format!(
            "ch{}:text{}:seg{}",
            self.chapter, self.text_index, self.segment_index
        )
    }

    /// Serialize as a 4000-char-clipped evidence payload.
    pub fn compact(&self) -> serde_json::Map<String, serde_json::Value> {
        const LIMIT: usize = 4000;
        fn clip(text: &str) -> String {
            text.chars().take(LIMIT).collect()
        }
        let mut payload = serde_json::Map::new();
        payload.insert("ref".to_string(), json!(self.reference()));
        payload.insert("chapter".to_string(), json!(self.chapter));
        payload.insert("text_index".to_string(), json!(self.text_index));
        payload.insert("segment_index".to_string(), json!(self.segment_index));
        payload.insert(
            "chapter_title".to_string(),
            json!(self.chapter_title.clone()),
        );
        payload.insert("source".to_string(), json!(clip(&self.source)));
        payload.insert("target".to_string(), json!(clip(&self.target)));
        payload.insert(
            "target_origin".to_string(),
            json!(self.target_origin.clone()),
        );
        payload.insert(
            "target_truncated".to_string(),
            json!(self.target.chars().count() > LIMIT),
        );
        if self.target_origin == "shadow_override" {
            payload.insert(
                "baseline_target".to_string(),
                json!(clip(&self.baseline_target)),
            );
            payload.insert(
                "baseline_target_truncated".to_string(),
                json!(self.baseline_target.chars().count() > LIMIT),
            );
        }
        payload
    }
}

/// Validated shadow candidates (oracle `ReviewFixPlan`).
#[derive(Debug, Clone, Default)]
pub struct ReviewFixPlan {
    /// Proposed patches.
    pub patches: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Fix failures.
    pub failures: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Applicable patches (before-hash verified).
    pub applicable: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Candidate overrides by (chapter, text_index).
    pub overrides: BTreeMap<(i64, i64), String>,
    /// Overlay digest with candidates applied.
    pub candidate_digest: String,
    /// Issue key per issue id.
    pub issue_keys_by_id: BTreeMap<String, String>,
}

/// Deterministic result of one whole-book round (oracle `ReviewRoundResult`).
#[derive(Debug, Clone, Default)]
pub struct ReviewRoundResult {
    /// Final issues.
    pub issues: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Pre-arbitration issues.
    pub pre_arbitration_issues: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Superseded by arbitration.
    pub arbitration_superseded: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Conflict groups.
    pub conflict_groups: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Unresolved conflicts.
    pub residual_conflicts: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Agent-loop fallbacks used.
    pub fallback_agent_count: i64,
}

/// Mutable shadow state for one review (oracle `ReviewSessionState`).
#[derive(Debug, Clone)]
pub struct ReviewSessionState {
    /// Active shadow overrides by (chapter, text_index).
    pub target_overrides: BTreeMap<(i64, i64), String>,
    /// Overlay digests already seen (cycle detection).
    pub seen_overlays: BTreeSet<String>,
    /// Patch history records.
    pub patch_records: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Active patch per location.
    pub active_patches: BTreeMap<(i64, i64), serde_json::Map<String, serde_json::Value>>,
    /// Fix failures.
    pub fix_failures: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Blocked issues by issue key.
    pub blocked_issues: BTreeMap<String, serde_json::Map<String, serde_json::Value>>,
    /// Round summaries.
    pub round_summaries: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Latest round result.
    pub latest: Option<ReviewRoundResult>,
    /// Consecutive clean rounds.
    pub clean_streak: i64,
    /// Completed fix rounds.
    pub fix_rounds: i64,
    /// Termination reason.
    pub termination: String,
}

impl Default for ReviewSessionState {
    fn default() -> Self {
        Self {
            target_overrides: BTreeMap::new(),
            seen_overlays: BTreeSet::new(),
            patch_records: Vec::new(),
            active_patches: BTreeMap::new(),
            fix_failures: Vec::new(),
            blocked_issues: BTreeMap::new(),
            round_summaries: Vec::new(),
            latest: None,
            clean_streak: 0,
            fix_rounds: 0,
            termination: "not_started".to_string(),
        }
    }
}

/// Fingerprint effective shadow text (oracle `review_overlay_digest`).
pub fn review_overlay_digest(
    chapters: &[(i64, Vec<String>)],
    overrides: &BTreeMap<(i64, i64), String>,
) -> String {
    let mut payload = Vec::new();
    for (chapter, targets) in chapters {
        for (text_index, target) in targets.iter().enumerate() {
            let effective = overrides
                .get(&(*chapter, text_index as i64))
                .cloned()
                .unwrap_or_else(|| target.clone());
            payload.push(json!([chapter, text_index, effective]));
        }
    }
    text_hash(&serde_json::Value::Array(payload).to_string())
}

/// Formal chapter text: (chapter, [(segment_index, anchor, kind, source, target)]).
pub type FormalChapters = Vec<(i64, Vec<(i64, String, String, String, String)>)>;

/// Hash the formal body text actually read by the review.
#[allow(clippy::type_complexity)] // Shape documented by `FormalChapters`.
pub fn content_digest(chapters: &[(i64, Vec<(i64, String, String, String, String)>)]) -> String {
    // (chapter, [(segment_index, anchor, kind, source, target)])
    let mut payload = Vec::new();
    for (chapter, segments) in chapters {
        for (text_index, (segment_index, anchor, kind, source, target)) in
            segments.iter().enumerate()
        {
            payload.push(json!([
                chapter,
                text_index,
                segment_index,
                anchor,
                kind,
                source,
                target
            ]));
        }
    }
    text_hash(&serde_json::Value::Array(payload).to_string())
}

/// Confirmation and revision limits (oracle `ReviewPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewPolicy {
    /// Fix loop enabled.
    pub fix_loop: bool,
    /// Max provisional fix rounds.
    pub max_fix_rounds: i64,
    /// Consecutive clean passes required.
    pub clean_confirmations: i64,
}

impl ReviewPolicy {
    /// Required clean passes (1 when the loop is off).
    pub fn required_clean(self) -> i64 {
        if self.fix_loop {
            self.clean_confirmations
        } else {
            1
        }
    }

    /// Max whole-book rounds.
    pub fn max_review_rounds(self) -> i64 {
        if self.fix_loop {
            (self.max_fix_rounds + 1) * self.required_clean()
        } else {
            1
        }
    }
}

/// Next step plus round summary (oracle `ReviewDecision`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDecision {
    /// `continue`/`fix`/`stop`.
    pub action: String,
    /// Round summary payload.
    pub summary: serde_json::Map<String, serde_json::Value>,
    /// Updated clean progress, when changed.
    pub clean_progress: Option<i64>,
}

/// Verified result of one leaf block (oracle `ReviewLoopOutcome`).
#[derive(Debug, Clone, Default)]
pub struct ReviewLoopOutcome {
    /// Kept issues.
    pub issues: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Dismissed candidates.
    pub dismissed: Vec<serde_json::Map<String, serde_json::Value>>,
    /// Fallback reason (empty when verified).
    pub fallback_reason: String,
}

impl ReviewSessionState {
    /// Retain fixer failures by stable issue key (oracle `register_blocked`).
    pub fn register_blocked(
        &mut self,
        issues: &[serde_json::Map<String, serde_json::Value>],
        failures: &[serde_json::Map<String, serde_json::Value>],
    ) {
        let by_id: BTreeMap<String, &serde_json::Map<String, serde_json::Value>> = issues
            .iter()
            .filter_map(|issue| {
                issue
                    .get("issue_id")
                    .and_then(|v| v.as_str())
                    .map(|id| (id.to_string(), issue))
            })
            .collect();
        for failure in failures {
            let ids: Vec<String> = match failure.get("issue_ids") {
                Some(serde_json::Value::Array(items)) => items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
                _ => failure
                    .get("issue_id")
                    .and_then(|v| v.as_str())
                    .map(|id| vec![id.to_string()])
                    .unwrap_or_default(),
            };
            for issue_id in ids {
                let Some(issue) = by_id.get(&issue_id) else {
                    continue;
                };
                let Some(issue_key) = issue.get("issue_key").and_then(|v| v.as_str()) else {
                    continue;
                };
                if issue_key.is_empty() {
                    continue;
                }
                let mut blocked = (*issue).clone();
                let mut fix_failure = serde_json::Map::new();
                for key in ["status", "reason", "review_round"] {
                    if let Some(value) = failure.get(key) {
                        fix_failure.insert(key.to_string(), value.clone());
                    }
                }
                blocked.insert(
                    "fix_failure".to_string(),
                    serde_json::Value::Object(fix_failure),
                );
                self.blocked_issues.insert(issue_key.to_string(), blocked);
            }
        }
    }

    /// Merge current + historical unfixed issues in book order.
    pub fn effective_issues(
        &self,
        current: &ReviewRoundResult,
    ) -> Vec<serde_json::Map<String, serde_json::Value>> {
        let mut combined: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
            BTreeMap::new();
        for issue in &current.issues {
            if let Some(key) = issue.get("issue_key").and_then(|v| v.as_str()) {
                combined.insert(key.to_string(), issue.clone());
            }
        }
        for (issue_key, blocked) in &self.blocked_issues {
            match combined.get_mut(issue_key) {
                None => {
                    combined.insert(issue_key.clone(), blocked.clone());
                }
                Some(current_issue) => {
                    if let Some(failure) = blocked.get("fix_failure") {
                        current_issue.insert("fix_failure".to_string(), failure.clone());
                    }
                }
            }
        }
        let mut out: Vec<serde_json::Map<String, serde_json::Value>> =
            combined.into_values().collect();
        out.sort_by(|a, b| {
            let key = |issue: &serde_json::Map<String, serde_json::Value>| {
                (
                    issue.get("chapter").and_then(|v| v.as_i64()).unwrap_or(-1),
                    issue.get("index").and_then(|v| v.as_i64()).unwrap_or(-1),
                    issue
                        .get("review_round")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(-1),
                    issue
                        .get("issue_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                )
            };
            key(a).cmp(&key(b))
        });
        out
    }

    /// Apply blind-recheck evidence; choose stop/continue/shadow revision.
    pub fn after_scan(
        &mut self,
        current: &ReviewRoundResult,
        review_round: i64,
        overlay_digest: &str,
        policy: ReviewPolicy,
    ) -> ReviewDecision {
        let current_keys: BTreeSet<String> = current
            .issues
            .iter()
            .filter_map(|issue| {
                issue
                    .get("issue_key")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();
        for patch_record in self.active_patches.values_mut() {
            let round = patch_record
                .get("round")
                .and_then(|v| v.as_i64())
                .unwrap_or(review_round);
            if round >= review_round {
                continue;
            }
            let covered: BTreeSet<String> = patch_record
                .get("issue_keys")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let rereported: Vec<String> = covered.intersection(&current_keys).cloned().collect();
            let not_rereported: Vec<String> = covered.difference(&current_keys).cloned().collect();
            for key in &not_rereported {
                self.blocked_issues.remove(key);
            }
            patch_record.insert(
                "rereported_issue_keys".to_string(),
                serde_json::Value::Array(
                    rereported
                        .iter()
                        .map(|k| serde_json::Value::String(k.clone()))
                        .collect(),
                ),
            );
            patch_record.insert(
                "not_rereported_issue_keys".to_string(),
                serde_json::Value::Array(
                    not_rereported
                        .iter()
                        .map(|k| serde_json::Value::String(k.clone()))
                        .collect(),
                ),
            );
            if !rereported.is_empty() {
                patch_record.insert("status".to_string(), json!("needs_revision"));
                patch_record.insert("failed_review_round".to_string(), json!(review_round));
            } else {
                if patch_record.get("status").and_then(|v| v.as_str()) != Some("not_rereported") {
                    patch_record.insert("not_rereported_in_round".to_string(), json!(review_round));
                }
                patch_record.insert("status".to_string(), json!("not_rereported"));
            }
        }
        let mut summary = serde_json::Map::new();
        summary.insert("review_round".to_string(), json!(review_round));
        summary.insert("overlay_digest".to_string(), json!(overlay_digest));
        summary.insert(
            "override_count".to_string(),
            json!(self.target_overrides.len() as i64),
        );
        summary.insert(
            "issue_count".to_string(),
            json!(current.issues.len() as i64),
        );
        summary.insert(
            "conflict_count".to_string(),
            json!(current.conflict_groups.len() as i64),
        );
        summary.insert(
            "unresolved_conflict_count".to_string(),
            json!(current.residual_conflicts.len() as i64),
        );
        summary.insert(
            "fallback_agent_count".to_string(),
            json!(current.fallback_agent_count),
        );
        summary.insert("clean_streak_before".to_string(), json!(self.clean_streak));
        summary.insert(
            "blocked_issue_count".to_string(),
            json!(self.blocked_issues.len() as i64),
        );
        if current.issues.is_empty() {
            if !self.blocked_issues.is_empty() {
                self.clean_streak = 0;
                self.termination = "unresolved_fixes".to_string();
                summary.insert("clean_streak_after".to_string(), json!(0));
                summary.insert("patch_count".to_string(), json!(0));
                summary.insert("termination".to_string(), json!(self.termination.clone()));
                return ReviewDecision {
                    action: "stop".to_string(),
                    summary,
                    clean_progress: Some(0),
                };
            }
            self.clean_streak += 1;
            summary.insert("clean_streak_after".to_string(), json!(self.clean_streak));
            summary.insert("patch_count".to_string(), json!(0));
            if self.clean_streak >= policy.required_clean() {
                self.termination = "clean_confirmed".to_string();
                summary.insert("termination".to_string(), json!(self.termination.clone()));
            }
            let action = if self.termination == "clean_confirmed" {
                "stop"
            } else {
                "continue"
            };
            return ReviewDecision {
                action: action.to_string(),
                summary,
                clean_progress: Some(self.clean_streak),
            };
        }
        let clean_progress = if self.clean_streak == 0 {
            None
        } else {
            Some(0)
        };
        self.clean_streak = 0;
        summary.insert("clean_streak_after".to_string(), json!(0));
        if !policy.fix_loop {
            self.termination = "issues_reported".to_string();
        } else if self.fix_rounds >= policy.max_fix_rounds {
            self.termination = "max_rounds".to_string();
        } else {
            return ReviewDecision {
                action: "fix".to_string(),
                summary,
                clean_progress,
            };
        }
        summary.insert("patch_count".to_string(), json!(0));
        summary.insert("termination".to_string(), json!(self.termination.clone()));
        ReviewDecision {
            action: "stop".to_string(),
            summary,
            clean_progress,
        }
    }

    /// Validate candidate bases; record blocked failures (oracle `prepare_fix`).
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_fix(
        &mut self,
        latest: &ReviewRoundResult,
        patches: Vec<serde_json::Map<String, serde_json::Value>>,
        mut failures: Vec<serde_json::Map<String, serde_json::Value>>,
        current_targets: &BTreeMap<(i64, i64), String>,
        chapters: &[(i64, Vec<String>)],
        review_round: i64,
        round_summary: &mut serde_json::Map<String, serde_json::Value>,
    ) -> Option<ReviewFixPlan> {
        let stamped: Vec<serde_json::Map<String, serde_json::Value>> = failures
            .iter()
            .map(|failure| {
                let mut stamped = failure.clone();
                stamped.insert("review_round".to_string(), json!(review_round));
                stamped
            })
            .collect();
        self.fix_failures.extend(stamped.clone());
        self.register_blocked(&latest.issues, &stamped);
        round_summary.insert("patch_count".to_string(), json!(patches.len() as i64));
        if patches.is_empty() {
            self.termination = "no_progress".to_string();
            round_summary.insert(
                "fix_failure_count".to_string(),
                json!(failures.len() as i64),
            );
            round_summary.insert(
                "blocked_issue_count".to_string(),
                json!(self.blocked_issues.len() as i64),
            );
            round_summary.insert("termination".to_string(), json!(self.termination.clone()));
            return None;
        }
        let issue_keys_by_id: BTreeMap<String, String> = latest
            .issues
            .iter()
            .filter_map(|issue| {
                match (
                    issue.get("issue_id").and_then(|v| v.as_str()),
                    issue.get("issue_key").and_then(|v| v.as_str()),
                ) {
                    (Some(id), Some(key)) => Some((id.to_string(), key.to_string())),
                    _ => None,
                }
            })
            .collect();
        let mut candidate_overrides = self.target_overrides.clone();
        let mut applicable = Vec::new();
        let mut hash_failures = Vec::new();
        for patch in &patches {
            let location = (
                patch.get("chapter").and_then(|v| v.as_i64()).unwrap_or(-1),
                patch.get("index").and_then(|v| v.as_i64()).unwrap_or(-1),
            );
            let before_hash = patch
                .get("before_hash")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let matches = current_targets
                .get(&location)
                .is_some_and(|current| text_hash(current) == before_hash);
            if !matches {
                let mut failure = serde_json::Map::new();
                failure.insert(
                    "patch_id".to_string(),
                    patch
                        .get("patch_id")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                );
                failure.insert(
                    "issue_ids".to_string(),
                    patch.get("issue_ids").cloned().unwrap_or(json!([])),
                );
                failure.insert("chapter".to_string(), json!(location.0));
                failure.insert("index".to_string(), json!(location.1));
                failure.insert("status".to_string(), json!("failed"));
                failure.insert("reason".to_string(), json!("before_hash_changed"));
                failure.insert("review_round".to_string(), json!(review_round));
                self.fix_failures.push(failure.clone());
                failures.push(failure.clone());
                hash_failures.push(failure);
                continue;
            }
            if let Some(after) = patch.get("after").and_then(|v| v.as_str()) {
                candidate_overrides.insert(location, after.to_string());
            }
            applicable.push(patch.clone());
        }
        self.register_blocked(&latest.issues, &hash_failures);
        round_summary.insert(
            "fix_failure_count".to_string(),
            json!(failures.len() as i64),
        );
        round_summary.insert(
            "blocked_issue_count".to_string(),
            json!(self.blocked_issues.len() as i64),
        );
        let candidate_digest = review_overlay_digest(chapters, &candidate_overrides);
        Some(ReviewFixPlan {
            patches,
            failures,
            applicable,
            overrides: candidate_overrides,
            candidate_digest,
            issue_keys_by_id,
        })
    }

    /// Reject no-progress/cyclic overlays or accept the shadow state.
    pub fn after_fix(
        &mut self,
        plan: &ReviewFixPlan,
        overlay_digest: &str,
        round_summary: &mut serde_json::Map<String, serde_json::Value>,
    ) -> bool {
        if plan.applicable.is_empty() || plan.candidate_digest == overlay_digest {
            self.termination = "no_progress".to_string();
            round_summary.insert("termination".to_string(), json!(self.termination.clone()));
            return false;
        }
        if self.seen_overlays.contains(&plan.candidate_digest) {
            self.termination = "cycle_detected".to_string();
            for patch in &plan.applicable {
                let mut record = patch.clone();
                let keys: BTreeSet<String> = patch
                    .get("issue_ids")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| v.as_str())
                            .filter_map(|id| plan.issue_keys_by_id.get(id).cloned())
                            .collect()
                    })
                    .unwrap_or_default();
                record.insert(
                    "issue_keys".to_string(),
                    serde_json::Value::Array(
                        keys.iter()
                            .map(|k| serde_json::Value::String(k.clone()))
                            .collect(),
                    ),
                );
                record.insert("status".to_string(), json!("rejected_cycle"));
                self.patch_records.push(record);
            }
            round_summary.insert("termination".to_string(), json!(self.termination.clone()));
            return false;
        }
        self.fix_rounds += 1;
        for patch in &plan.applicable {
            let location = (
                patch.get("chapter").and_then(|v| v.as_i64()).unwrap_or(-1),
                patch.get("index").and_then(|v| v.as_i64()).unwrap_or(-1),
            );
            if let Some(previous) = self.active_patches.get_mut(&location) {
                previous.insert("status".to_string(), json!("superseded"));
                if let Some(patch_id) = patch.get("patch_id") {
                    previous.insert("superseded_by".to_string(), patch_id.clone());
                }
            }
            let mut record = patch.clone();
            let keys: BTreeSet<String> = patch
                .get("issue_ids")
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .filter_map(|id| plan.issue_keys_by_id.get(id).cloned())
                        .collect()
                })
                .unwrap_or_default();
            record.insert(
                "issue_keys".to_string(),
                serde_json::Value::Array(
                    keys.iter()
                        .map(|k| serde_json::Value::String(k.clone()))
                        .collect(),
                ),
            );
            self.patch_records.push(record.clone());
            self.active_patches.insert(location, record);
        }
        self.target_overrides = plan.overrides.clone();
        self.seen_overlays.insert(plan.candidate_digest.clone());
        round_summary.insert("fix_round".to_string(), json!(self.fix_rounds));
        true
    }
}
