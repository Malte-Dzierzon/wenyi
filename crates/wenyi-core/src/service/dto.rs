//! Service DTOs: serde-owned shapes for the GUI (no transport fields).
//!
//! Field lists mirror the old REST `client.rs` DTOs minus transport-only
//! fields (`token`, `banner`, caches).

use serde::{Deserialize, Serialize};

/// Project list item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub title: Option<String>,
    pub fmt: Option<String>,
    pub source_lang: Option<String>,
    pub target_lang: Option<String>,
    pub status: String,
    pub error: Option<String>,
    #[serde(default)]
    pub initialized: bool,
    pub created_at: Option<String>,
}

/// Project detail with counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDetail {
    #[serde(flatten)]
    pub project: Project,
    #[serde(default)]
    pub book_title: Option<String>,
    #[serde(default)]
    pub chapter_count: u32,
    #[serde(default)]
    pub total_word_count: u64,
    #[serde(default)]
    pub done_chapters: u32,
}

/// Chapter summary row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChapterSummary {
    pub index: u32,
    #[serde(default)]
    pub title: String,
    pub title_translated: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub word_count: u64,
    #[serde(default)]
    pub target_word_count: u64,
    #[serde(default)]
    pub review_issue_count: u32,
    #[serde(default)]
    pub review_status: String,
}

/// One translated segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentOut {
    pub index: u32,
    pub source: String,
    pub target: Option<String>,
    pub target_before_polish: Option<String>,
    pub anchor: Option<String>,
    #[serde(default)]
    pub kind: String,
}

/// One chapter with segments (proofreading shape).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterSegments {
    pub index: u32,
    #[serde(default)]
    pub title: String,
    pub title_translated: Option<String>,
    #[serde(default)]
    pub segments: Vec<SegmentOut>,
    #[serde(default)]
    pub review_issues: Vec<serde_json::Value>,
}

/// One segment revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentRevision {
    pub id: String,
    pub kind: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// Workflow state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowOut {
    pub source: String,
    pub kind: String,
    pub status: String,
    pub run_id: Option<String>,
    pub review_id: Option<String>,
    #[serde(default)]
    pub stages: Vec<WorkflowStage>,
    pub progress: Option<serde_json::Value>,
    #[serde(default)]
    pub running_chapters: Vec<i64>,
}

/// One workflow stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowStage {
    pub id: String,
    pub label: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Glossary term.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlossaryTerm {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub reading: String,
    #[serde(default, rename = "type")]
    pub term_type: String,
    #[serde(default)]
    pub gender: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub first_chapter: Option<i64>,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub mentions: i64,
}

/// Glossary term input (add/update body).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TermIn {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub reading: String,
    #[serde(default = "default_term_type", rename = "type")]
    pub term_type: String,
    #[serde(default)]
    pub gender: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub note: String,
}

fn default_term_type() -> String {
    "term".into()
}

/// Glossary conflict row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermConflict {
    pub id: i64,
    pub source: String,
    pub existing_target: Option<String>,
    pub proposed_target: Option<String>,
    pub chapter: Option<i64>,
    pub note: Option<String>,
}

/// Analysis document + chapter digests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisOut {
    #[serde(default)]
    pub analysis: serde_json::Value,
    #[serde(default)]
    pub chapter_digests: Vec<ChapterDigest>,
}

/// One chapter digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterDigest {
    pub index: u32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub digest: String,
}

/// Export request body.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    pub bilingual: bool,
    pub order: String,
    pub about_page: bool,
    pub preserve_source_style: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub punctuation_normalize: Option<bool>,
    pub pdf_engine: String,
}

impl ExportRequest {
    /// Request defaults (mirrors the old REST client).
    pub fn new() -> Self {
        Self {
            format: None,
            bilingual: false,
            order: "target_first".into(),
            about_page: true,
            preserve_source_style: false,
            punctuation_normalize: None,
            pdf_engine: "weasyprint".into(),
        }
    }
}

/// Export record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportOut {
    #[serde(default)]
    pub options: serde_json::Value,
    pub error: Option<String>,
    pub id: String,
    pub project_id: String,
    pub format: String,
    pub status: String,
    pub path: Option<String>,
    pub size: Option<i64>,
    pub created_at: Option<String>,
}

/// Global config (YAML text + effective JSON).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfigOut {
    pub yaml: String,
    #[serde(default)]
    pub effective: serde_json::Value,
    pub default_template: String,
    pub revision: i64,
}

/// Static engine capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capabilities {
    pub languages: Vec<Language>,
    pub input_formats: Vec<String>,
    pub output_formats: Vec<String>,
    pub provider_kinds: Vec<String>,
    /// Model operations from the `llm::operations` registry (routing
    /// groups + tier defaults for the settings UI). Never empty: the
    /// registry is compiled in.
    #[serde(default)]
    pub operations: Vec<OperationCap>,
}

/// One model operation capability (routing group + tier default).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationCap {
    pub id: String,
    pub tier: String,
    pub description: String,
}

/// Background curator state for one project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratorStatus {
    pub running: bool,
    #[serde(default)]
    pub run_id: Option<String>,
    /// Latest checked term (from the newest `curator_term_checked` event).
    #[serde(default)]
    pub current_term: Option<String>,
    pub terms_total: i64,
    pub terms_checked: i64,
    pub open_conflicts: i64,
    /// Last pass stats from `curator_state` (`checked`/`verified`/...).
    #[serde(default)]
    pub last_pass: serde_json::Value,
    /// Newest `curator_pass_finished` payloads (max 5).
    #[serde(default)]
    pub recent_passes: Vec<serde_json::Value>,
}

/// Enqueued curator pass identities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratorStartOut {
    pub run_id: String,
    pub job_id: String,
}

/// One language.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Language {
    pub code: String,
    pub name: String,
}

/// Credential presence for one connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialStatus {
    pub connection: String,
    pub configured: bool,
    pub source: String,
}

/// One model-list ping result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPing {
    pub connection: String,
    pub ok: bool,
    pub models: Vec<String>,
    pub error: Option<String>,
}

/// UI-facing engine event (built on `jobs::ProgressEvent`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineEvent {
    pub project_id: String,
    pub run_id: String,
    pub kind: String,
    pub done: i64,
    pub total: i64,
    pub message: String,
}

impl From<&crate::jobs::ProgressEvent> for EngineEvent {
    fn from(event: &crate::jobs::ProgressEvent) -> Self {
        Self {
            project_id: event.project_id.as_str().to_string(),
            run_id: event.run_id.as_str().to_string(),
            kind: event.kind.clone(),
            done: event.done,
            total: event.total,
            message: event.message.clone(),
        }
    }
}

/// GUI-facing service error (maps 1:1 from [`crate::Error`]).
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// Entity not found.
    #[error("not found: {0}")]
    NotFound(String),
    /// Stale-baseline edit conflict.
    #[error("conflict: {0}")]
    Conflict(String),
    /// Invalid input or config.
    #[error("validation: {0}")]
    Validation(String),
    /// Lane busy / queue full.
    #[error("busy: {0}")]
    Busy(String),
    /// Engine failure.
    #[error("engine: {0}")]
    Engine(String),
}

impl From<crate::Error> for ServiceError {
    fn from(error: crate::Error) -> Self {
        match error {
            crate::Error::NotFound { kind, id } => Self::NotFound(format!("{kind} {id}")),
            crate::Error::Conflict(reason) => Self::Conflict(reason),
            crate::Error::Unsupported(message) => Self::Validation(message),
            crate::Error::Config(message) => Self::Validation(message),
            crate::Error::Stopped(message) => Self::Busy(message),
            crate::Error::Store(message) if is_busy_message(&message) => Self::Busy(message),
            crate::Error::Store(message)
            | crate::Error::Provider(message)
            | crate::Error::Io { message, .. } => Self::Engine(message),
        }
    }
}

fn is_busy_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("already has a running task")
        || lower.contains("conflicting running task")
        || lower.contains("curator pass is already running")
        || lower.contains("queue is full")
}

/// Project stats: usage + timing ledgers (raw JSON, UI formats).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectStats {
    /// `usage.json` ledger (empty object when no usage yet).
    #[serde(default)]
    pub usage: serde_json::Value,
    /// `timing.json` ledger (empty object when no runs yet).
    #[serde(default)]
    pub timing: serde_json::Value,
    /// Live progress override while a job runs.
    #[serde(default)]
    pub live: Option<serde_json::Value>,
}

/// One review run (summary form for lists).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewRunOut {
    /// Review identity.
    pub id: String,
    /// `running`/`completed`/`partial`/`interrupted`/`failed`.
    pub status: String,
    /// Termination reason.
    pub termination: String,
    /// Final issue list.
    #[serde(default)]
    pub issues: Vec<serde_json::Value>,
    /// Result summary.
    #[serde(default)]
    pub summary: serde_json::Value,
}

/// One persisted engine event (history form).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventOut {
    /// Monotonic row id (per project).
    pub id: i64,
    /// Owning job.
    pub job_id: String,
    /// Event time (epoch ms).
    pub ts: i64,
    /// Event kind.
    #[serde(default, rename = "type")]
    pub event_type: String,
    /// Event payload.
    #[serde(default)]
    pub payload: serde_json::Value,
}
