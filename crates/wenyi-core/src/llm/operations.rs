//! Model-driven operation registry. Oracle: `llm/operations.py`.
//!
//! The single registration point binding operation IDs to default tiers,
//! output hints, workflow reachability, and protocol versions.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::{Error, Result};
use serde::Serialize;

/// Product tiers every config must define.
pub const TIERS: [&str; 3] = ["strong", "cheap", "fast"];

/// One registered model-driven operation.
#[derive(Debug, Clone, Serialize)]
pub struct OperationSpec {
    /// Operation ID (`area.name`).
    pub id: &'static str,
    /// Human description.
    pub description: &'static str,
    /// Default tier (exactly one of tier/inherits is set).
    pub tier: Option<&'static str>,
    /// Parent operation whose route is inherited.
    pub inherits: Option<&'static str>,
    /// Default output-token hint.
    pub output_tokens: Option<i64>,
    /// Workflows that can reach this operation.
    pub workflows: &'static [&'static str],
    /// Pipeline flags gating reachability.
    pub flags: &'static [&'static str],
    /// Review-workflow operation.
    pub review: bool,
    /// Failover forbidden inside resumable evidence conversations.
    pub resumable_conversation: bool,
    /// Prompt protocol version (enters the inference fingerprint).
    pub protocol_version: i64,
}

macro_rules! op {
    ($id:literal, $desc:literal, tier = $tier:literal $(, $k:ident = $v:expr)* $(,)?) => {
        OperationSpec {
            id: $id,
            description: $desc,
            tier: Some($tier),
            inherits: None,
            $($k: $v,)*
            ..OperationSpec::translate_default()
        }
    };
    ($id:literal, $desc:literal, inherits = $parent:literal $(, $k:ident = $v:expr)* $(,)?) => {
        OperationSpec {
            id: $id,
            description: $desc,
            tier: None,
            inherits: Some($parent),
            $($k: $v,)*
            ..OperationSpec::translate_default()
        }
    };
}

impl OperationSpec {
    const fn translate_default() -> Self {
        Self {
            id: "",
            description: "",
            tier: None,
            inherits: None,
            output_tokens: None,
            workflows: &["translate"],
            flags: &[],
            review: false,
            resumable_conversation: false,
            protocol_version: 1,
        }
    }
}

/// All registered operations (oracle `OPERATIONS` order).
static OPERATIONS: LazyLock<BTreeMap<&'static str, OperationSpec>> = LazyLock::new(|| {
    let specs = [
        op!(
            "language.detect",
            "Detect source language",
            tier = "cheap",
            output_tokens = Some(512),
            workflows = &["prepare", "translate"],
            flags = &["language_auto"]
        ),
        op!(
            "analysis.style",
            "Analyze style, characters and seed terms",
            tier = "strong",
            output_tokens = Some(8192),
            workflows = &["prepare", "translate"]
        ),
        op!(
            "synopsis.chapter",
            "Summarize one chapter",
            tier = "fast",
            output_tokens = Some(8192),
            workflows = &["prepare", "translate"],
            flags = &["book_understanding"]
        ),
        op!(
            "synopsis.book",
            "Merge digests into a book synopsis",
            tier = "fast",
            output_tokens = Some(8192),
            workflows = &["prepare", "translate"],
            flags = &["book_understanding"]
        ),
        op!(
            "translation.body",
            "Translate body paragraphs",
            tier = "strong",
            output_tokens = Some(8192),
            protocol_version = 2
        ),
        op!(
            "translation.title",
            "Translate chapter and TOC titles",
            tier = "strong",
            output_tokens = Some(4096)
        ),
        op!(
            "polish.body",
            "Polish translated paragraphs",
            tier = "strong",
            output_tokens = Some(8192),
            flags = &["polish"],
            protocol_version = 2
        ),
        op!(
            "glossary.extract",
            "Extract glossary candidates",
            tier = "fast",
            output_tokens = Some(4096)
        ),
        op!(
            "glossary.align_history",
            "Align terms with earlier translations",
            tier = "fast",
            output_tokens = Some(4096)
        ),
        op!(
            "glossary.curate",
            "Review one glossary term against book evidence",
            tier = "cheap",
            output_tokens = Some(2048)
        ),
        op!(
            "annotation.align",
            "Align EPUB annotation positions",
            tier = "cheap",
            output_tokens = Some(4096),
            workflows = &["translate", "review"],
            flags = &["annotation_alignment"]
        ),
        op!(
            "review.scan",
            "Review and blindly re-review translated text",
            tier = "cheap",
            output_tokens = Some(8192),
            workflows = &["translate", "review"],
            review = true
        ),
        op!(
            "review.verify",
            "Verify issues with evidence",
            tier = "strong",
            output_tokens = Some(8192),
            workflows = &["translate", "review"],
            flags = &["review_agent_loop"],
            review = true,
            resumable_conversation = true
        ),
        op!(
            "review.arbitrate",
            "Arbitrate conflicting recommendations",
            tier = "strong",
            output_tokens = Some(8192),
            workflows = &["translate", "review"],
            flags = &["review_agent_loop", "review_conflict_arbitration"],
            review = true,
            resumable_conversation = true
        ),
        op!(
            "review.fix",
            "Propose shadow translation replacements",
            tier = "strong",
            output_tokens = Some(8192),
            workflows = &["translate", "review"],
            flags = &["review_fix_loop"],
            review = true,
            protocol_version = 2
        ),
        op!(
            "autofix.verify",
            "Verify issues before publication",
            inherits = "review.verify",
            workflows = &["translate", "review"],
            flags = &["review_autofix"],
            review = true,
            resumable_conversation = true
        ),
        op!(
            "autofix.fix",
            "Propose replacements for publication",
            inherits = "review.fix",
            workflows = &["translate", "review"],
            flags = &["review_autofix"],
            review = true,
            protocol_version = 2
        ),
        op!(
            "srt.translate",
            "Translate subtitle cues",
            tier = "strong",
            output_tokens = Some(8192),
            workflows = &["srt"]
        ),
        op!(
            "style.observe",
            "Propose style profile refinements from chapter evidence",
            tier = "cheap",
            output_tokens = Some(2048),
            workflows = &["translate"]
        ),
    ];
    let mut map = BTreeMap::new();
    for spec in specs {
        debug_assert!(
            (spec.tier.is_none()) != (spec.inherits.is_none()),
            "operation {} needs exactly one default selection",
            spec.id
        );
        map.insert(spec.id, spec);
    }
    map
});

/// Look up a registered operation, rejecting unknown IDs.
pub fn require_operation(operation: &str) -> Result<&'static OperationSpec> {
    OPERATIONS
        .get(operation)
        .ok_or_else(|| Error::Config(format!("Unknown model operation: {operation}")))
}

/// All registered operation IDs in registry order.
pub fn operation_ids() -> Vec<&'static str> {
    // BTreeMap iterates sorted; the oracle preserves declaration order, but
    // resolution order is irrelevant (memoized recursion). Golden tests pin
    // the sorted ID list explicitly.
    OPERATIONS.keys().copied().collect()
}

/// Operations reachable in a workflow given pipeline flags (oracle `workflow_operations`).
pub fn workflow_operations(
    workflow: &str,
    flags: &BTreeMap<String, bool>,
) -> Result<Vec<&'static str>> {
    if !["prepare", "translate", "review", "srt"].contains(&workflow) {
        return Err(Error::Config(format!("Unknown model workflow: {workflow}")));
    }
    let flag = |name: &str| flags.get(name).copied().unwrap_or(true);
    Ok(OPERATIONS
        .values()
        .filter(|spec| {
            spec.workflows.contains(&workflow)
                && (!spec.review || workflow == "review" || flag("review"))
                && spec.flags.iter().all(|f| flag(f))
        })
        .map(|spec| spec.id)
        .collect())
}
