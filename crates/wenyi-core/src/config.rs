//! Configuration parsing with fixture parity against the Python oracle.
//!
//! Oracle: `packages/core/wenyi_core/config.py` (`Config::from_dict`) and
//! `llm/configuration.py`. Fixtures in `tests/fixtures/config_*.yaml` with
//! expectations in `config_expected.json` (generated from the oracle).
//!
//! Known Slice 1 gap: provider-kind-specific validation (`requires_base_url`,
//! adapter option schemas) lands with the Slice 3 provider adapters. Unknown
//! provider kinds are rejected explicitly; they are never silently accepted.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{Error, Result};

/// Tiers every valid routing graph must define exactly.
pub const TIERS: [&str; 3] = ["strong", "cheap", "fast"];

/// Provider kinds with built-in presets understood by this module.
const PRESET_KINDS: [&str; 3] = ["deepseek", "fake", "gemini"];

/// Parse YAML text into an unvalidated generic value.
///
/// The concrete YAML crate is chosen by the spike test (`tests/yaml_spike.rs`);
/// this module only sees `serde_yaml::Value`-shaped data via JSON conversion.
pub fn parse_yaml(text: &str) -> Result<serde_json::Value> {
    let value: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| Error::Config(e.to_string()))?;
    serde_json::to_value(&value).map_err(|e| Error::Config(e.to_string()))
}

/// Load and validate a full configuration document.
pub fn load(text: &str) -> Result<Config> {
    let raw = parse_yaml(text)?;
    Config::from_value(&raw)
}

fn env_name_valid(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('A'..='Z' | 'a'..='z' | '_'))
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn section_id_valid(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('A'..='Z' | 'a'..='z'))
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn base_url_valid(url: &str) -> bool {
    // Mirror of ProviderConfig.validate_url: absolute http(s), host, no
    // userinfo/query/fragment. Minimal parser: no url crate needed.
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"));
    let Some(rest) = rest else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    if url.contains(['?', '#', '@']) {
        return false;
    }
    // Host = up to first '/' or ':'; must be non-empty.
    let host = rest.split(['/', ':']).next().unwrap_or("");
    !host.is_empty()
}

/// Top-level runtime configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Source language code or `auto`.
    #[serde(default = "default_source")]
    pub source_lang: String,
    /// Target language code.
    #[serde(default = "default_target")]
    pub target_lang: String,
    /// LLM routing graph.
    #[serde(default)]
    pub llm: LlmConfig,
    /// Segmentation budgets.
    #[serde(default)]
    pub segment: SegmentConfig,
    /// Pipeline switches.
    #[serde(default)]
    pub pipeline: PipelineConfig,
    /// Output selection.
    #[serde(default)]
    pub output: OutputConfig,
    /// Honorific strategy name.
    #[serde(default = "default_honorific")]
    pub honorific_strategy: String,
    /// File-state directory (oracle compat; the Rust store uses SQLite).
    #[serde(default = "default_state_dir")]
    pub state_dir: String,
}

fn default_source() -> String {
    "auto".to_string()
}
fn default_target() -> String {
    "zh".to_string()
}
fn default_honorific() -> String {
    "keep_style".to_string()
}
fn default_state_dir() -> String {
    "state".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            source_lang: default_source(),
            target_lang: default_target(),
            llm: LlmConfig::default(),
            segment: SegmentConfig::default(),
            pipeline: PipelineConfig::default(),
            output: OutputConfig::default(),
            honorific_strategy: default_honorific(),
            state_dir: default_state_dir(),
        }
    }
}

impl Config {
    /// Build from a parsed YAML mapping, mirroring `Config.from_dict`.
    pub fn from_value(raw: &serde_json::Value) -> Result<Self> {
        let obj = raw.as_object().ok_or_else(|| {
            Error::Config("Configuration must be a mapping of sections.".to_string())
        })?;
        let known = [
            "language",
            "llm",
            "segment",
            "pipeline",
            "output",
            "honorific",
            "paths",
        ];
        let mut unknown: Vec<&str> = obj.keys().map(String::as_str).collect();
        unknown.retain(|k| !known.contains(k));
        if !unknown.is_empty() {
            unknown.sort_unstable();
            return Err(Error::Config(format!(
                "Unknown configuration sections: {}",
                unknown.join(", ")
            )));
        }
        let lang = obj.get("language").unwrap_or(&serde_json::Value::Null);
        let (source, target) = match lang {
            serde_json::Value::Null => (default_source(), default_target()),
            serde_json::Value::Object(map) => {
                if map.keys().any(|k| k != "source" && k != "target") {
                    return Err(Error::Config(
                        "Invalid or unknown language configuration fields".to_string(),
                    ));
                }
                (
                    string_field(map, "source")?.unwrap_or_else(default_source),
                    string_field(map, "target")?.unwrap_or_else(default_target),
                )
            }
            _ => {
                return Err(Error::Config(
                    "Invalid or unknown language configuration fields".to_string(),
                ))
            }
        };
        let llm = LlmConfig::from_value(obj.get("llm").unwrap_or(&serde_json::Value::Null))?;
        let segment =
            SegmentConfig::from_value(obj.get("segment").unwrap_or(&serde_json::Value::Null))?;
        let pipeline =
            PipelineConfig::from_value(obj.get("pipeline").unwrap_or(&serde_json::Value::Null))?;
        let output =
            OutputConfig::from_value(obj.get("output").unwrap_or(&serde_json::Value::Null))?;
        let honorific_strategy = obj
            .get("honorific")
            .and_then(|v| v.get("strategy"))
            .and_then(|v| v.as_str())
            .unwrap_or("keep_style")
            .to_string();
        let state_dir = obj
            .get("paths")
            .and_then(|v| v.get("state_dir"))
            .and_then(|v| v.as_str())
            .unwrap_or("state")
            .to_string();
        let cfg = Self {
            source_lang: source,
            target_lang: target,
            llm,
            segment,
            pipeline,
            output,
            honorific_strategy,
            state_dir,
        };
        cfg.validate_batch_fits_model_limits()?;
        Ok(cfg)
    }

    fn validate_batch_fits_model_limits(&self) -> Result<()> {
        let smallest = self
            .llm
            .models
            .values()
            .filter_map(|m| m.max_input_tokens)
            .min();
        if let Some(limit) = smallest {
            if self.segment.max_tokens_per_batch >= limit {
                return Err(Error::Config(format!(
                    "max_tokens_per_batch={} does not fit the smallest model input limit \
                     ({} tokens); lower the batch size or raise the model limit",
                    self.segment.max_tokens_per_batch, limit
                )));
            }
        }
        Ok(())
    }
}

fn string_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>> {
    match map.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(Error::Config(format!("{key} must be a string"))),
    }
}

fn object_or_null(
    value: &serde_json::Value,
    what: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    match value {
        serde_json::Value::Null => Ok(serde_json::Map::new()),
        serde_json::Value::Object(map) => Ok(map.clone()),
        _ => Err(Error::Config(format!("{what} must be a mapping"))),
    }
}

fn bool_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    default: bool,
) -> Result<bool> {
    match map.get(key) {
        None | Some(serde_json::Value::Null) => Ok(default),
        Some(serde_json::Value::Bool(b)) => Ok(*b),
        Some(_) => Err(Error::Config(format!("{key} must be a boolean"))),
    }
}

fn i64_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    default: i64,
) -> Result<i64> {
    match map.get(key) {
        None | Some(serde_json::Value::Null) => Ok(default),
        Some(serde_json::Value::Number(n)) => n
            .as_i64()
            .ok_or_else(|| Error::Config(format!("{key} must be an integer"))),
        Some(_) => Err(Error::Config(format!("{key} must be an integer"))),
    }
}

fn opt_i64_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<i64>> {
    match map.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(n)) => n
            .as_i64()
            .map(Some)
            .ok_or_else(|| Error::Config(format!("{key} must be an integer"))),
        Some(_) => Err(Error::Config(format!("{key} must be an integer"))),
    }
}

fn f64_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    default: f64,
) -> Result<f64> {
    match map.get(key) {
        None | Some(serde_json::Value::Null) => Ok(default),
        Some(serde_json::Value::Number(n)) => n
            .as_f64()
            .ok_or_else(|| Error::Config(format!("{key} must be a number"))),
        Some(_) => Err(Error::Config(format!("{key} must be a number"))),
    }
}

fn opt_f64_field(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<f64>> {
    match map.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(n)) => n
            .as_f64()
            .map(Some)
            .ok_or_else(|| Error::Config(format!("{key} must be a number"))),
        Some(_) => Err(Error::Config(format!("{key} must be a number"))),
    }
}

fn check_extra(
    map: &serde_json::Map<String, serde_json::Value>,
    known: &[&str],
    what: &str,
) -> Result<()> {
    let mut extra: Vec<&str> = map.keys().map(String::as_str).collect();
    extra.retain(|k| !known.contains(k));
    if extra.is_empty() {
        Ok(())
    } else {
        extra.sort_unstable();
        Err(Error::Config(format!(
            "Unknown {what} fields: {}",
            extra.join(", ")
        )))
    }
}

/// Segmentation budgets in `cl100k_base` tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentConfig {
    /// Target batch size in tokens.
    pub max_tokens_per_batch: i64,
    /// Split longer paragraphs at this budget.
    pub max_tokens_per_segment: i64,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            max_tokens_per_batch: 1800,
            max_tokens_per_segment: 1200,
        }
    }
}

impl SegmentConfig {
    fn from_value(raw: &serde_json::Value) -> Result<Self> {
        let map = object_or_null(raw, "segment")?;
        check_extra(
            &map,
            &["max_tokens_per_batch", "max_tokens_per_segment"],
            "segment",
        )?;
        Ok(Self {
            max_tokens_per_batch: i64_field(&map, "max_tokens_per_batch", 1800)?,
            max_tokens_per_segment: i64_field(&map, "max_tokens_per_segment", 1200)?,
        })
    }
}

/// Translation mode selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranslationMode {
    /// One draft per batch.
    #[default]
    Standard,
    /// Three drafts plus synthesis.
    BestOfThree,
}

/// Pipeline switches and budgets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// Translation mode.
    pub translation_mode: TranslationMode,
    /// Run whole-book review.
    pub review: bool,
    /// Structural retries for malformed model output.
    pub align_retry_limit: i64,
    /// Polish with the strong tier.
    pub polish: bool,
    /// Recent translated segments per batch.
    pub rolling_context_segments: i64,
    /// Prescan digests and synopsis.
    pub book_understanding: bool,
    /// Observe completed chapters into style proposals (cheap tier).
    pub style_observation: bool,
    /// Concurrent chapter-digest workers.
    pub prescan_concurrency: i64,
    /// Align EPUB annotation links after finalization.
    pub annotation_alignment: bool,
    /// Concurrent per-annotation alignment requests.
    pub annotation_alignment_concurrency: i64,
    /// Concurrent review blocks.
    pub review_concurrency: i64,
    /// Extra retries for malformed single-paragraph review output (0..=5).
    pub review_output_retries: i64,
    /// Bounded evidence agent loop.
    pub review_agent_loop: bool,
    /// Max evidence rounds per agent loop (0..=2).
    pub review_agent_max_evidence_rounds: i64,
    /// Arbitrate contradictory consistency proposals.
    pub review_conflict_arbitration: bool,
    /// Revise the shadow translation and blindly re-review.
    pub review_fix_loop: bool,
    /// Max provisional fix rounds (0..=4).
    pub review_fix_max_rounds: i64,
    /// Consecutive clean passes required (1..=2).
    pub review_clean_confirmations: i64,
    /// Publish formal translations after review.
    pub review_autofix: bool,
    /// PDF input backend.
    pub pdf_backend: PdfBackend,
    /// BabelDOC bridge base URL.
    pub babeldoc_bridge_url: String,
    /// Optional 1-based page selection.
    pub babeldoc_pages: Option<String>,
    /// Bridge HTTP timeout in seconds.
    pub babeldoc_timeout: f64,
}

/// PDF input backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfBackend {
    /// MinerU HTML path.
    Mineru,
    /// External AGPL HTTP bridge.
    Babeldoc,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            translation_mode: TranslationMode::Standard,
            review: true,
            align_retry_limit: 2,
            polish: true,
            rolling_context_segments: 6,
            book_understanding: true,
            style_observation: true,
            prescan_concurrency: 4,
            annotation_alignment: true,
            annotation_alignment_concurrency: 4,
            review_concurrency: 4,
            review_output_retries: 2,
            review_agent_loop: true,
            review_agent_max_evidence_rounds: 2,
            review_conflict_arbitration: true,
            review_fix_loop: true,
            review_fix_max_rounds: 2,
            review_clean_confirmations: 2,
            review_autofix: true,
            pdf_backend: PdfBackend::Mineru,
            babeldoc_bridge_url: "http://127.0.0.1:8765".to_string(),
            babeldoc_pages: None,
            babeldoc_timeout: 600.0,
        }
    }
}

const PIPELINE_FIELDS: [&str; 23] = [
    "translation_mode",
    "review",
    "align_retry_limit",
    "polish",
    "rolling_context_segments",
    "book_understanding",
    "style_observation",
    "prescan_concurrency",
    "annotation_alignment",
    "annotation_alignment_concurrency",
    "review_concurrency",
    "review_output_retries",
    "review_agent_loop",
    "review_agent_max_evidence_rounds",
    "review_conflict_arbitration",
    "review_fix_loop",
    "review_fix_max_rounds",
    "review_clean_confirmations",
    "review_autofix",
    "pdf_backend",
    "babeldoc_bridge_url",
    "babeldoc_pages",
    "babeldoc_timeout",
];

impl PipelineConfig {
    fn from_value(raw: &serde_json::Value) -> Result<Self> {
        let mut map = object_or_null(raw, "pipeline")?;
        // Legacy tolerance: read but ignore the retired key (oracle parity).
        map.remove("precision_concurrency");
        check_extra(&map, &PIPELINE_FIELDS, "pipeline")?;
        let defaults = Self::default();
        let translation_mode = match map.get("translation_mode") {
            None | Some(serde_json::Value::Null) => TranslationMode::Standard,
            Some(serde_json::Value::String(s)) => match s.as_str() {
                "standard" => TranslationMode::Standard,
                "best_of_three" => TranslationMode::BestOfThree,
                _ => {
                    return Err(Error::Config(format!(
                        "translation_mode must be standard or best_of_three, got {s:?}"
                    )))
                }
            },
            Some(_) => {
                return Err(Error::Config(
                    "translation_mode must be a string".to_string(),
                ))
            }
        };
        let polish = bool_field(&map, "polish", true)?;
        if translation_mode == TranslationMode::BestOfThree && !polish {
            return Err(Error::Config(
                "best_of_three translation mode requires pipeline.polish=true".to_string(),
            ));
        }
        let review_output_retries = i64_field(&map, "review_output_retries", 2)?;
        if !(0..=5).contains(&review_output_retries) {
            return Err(Error::Config(
                "review_output_retries must be 0..=5".to_string(),
            ));
        }
        let review_agent_max_evidence_rounds =
            i64_field(&map, "review_agent_max_evidence_rounds", 2)?;
        if !(0..=2).contains(&review_agent_max_evidence_rounds) {
            return Err(Error::Config(
                "review_agent_max_evidence_rounds must be 0..=2".to_string(),
            ));
        }
        let review_fix_max_rounds = i64_field(&map, "review_fix_max_rounds", 2)?;
        if !(0..=4).contains(&review_fix_max_rounds) {
            return Err(Error::Config(
                "review_fix_max_rounds must be 0..=4".to_string(),
            ));
        }
        let review_clean_confirmations = i64_field(&map, "review_clean_confirmations", 2)?;
        if !(1..=2).contains(&review_clean_confirmations) {
            return Err(Error::Config(
                "review_clean_confirmations must be 1..=2".to_string(),
            ));
        }
        let pdf_backend = match map.get("pdf_backend") {
            None | Some(serde_json::Value::Null) => PdfBackend::Mineru,
            Some(serde_json::Value::String(s)) => match s.as_str() {
                "mineru" => PdfBackend::Mineru,
                "babeldoc" => PdfBackend::Babeldoc,
                _ => {
                    return Err(Error::Config(format!(
                        "pdf_backend must be mineru or babeldoc, got {s:?}"
                    )))
                }
            },
            Some(_) => return Err(Error::Config("pdf_backend must be a string".to_string())),
        };
        Ok(Self {
            translation_mode,
            review: bool_field(&map, "review", defaults.review)?,
            align_retry_limit: i64_field(&map, "align_retry_limit", defaults.align_retry_limit)?,
            polish,
            rolling_context_segments: i64_field(
                &map,
                "rolling_context_segments",
                defaults.rolling_context_segments,
            )?,
            book_understanding: bool_field(
                &map,
                "book_understanding",
                defaults.book_understanding,
            )?,
            style_observation: bool_field(&map, "style_observation", defaults.style_observation)?,
            prescan_concurrency: i64_field(
                &map,
                "prescan_concurrency",
                defaults.prescan_concurrency,
            )?,
            annotation_alignment: bool_field(
                &map,
                "annotation_alignment",
                defaults.annotation_alignment,
            )?,
            annotation_alignment_concurrency: i64_field(
                &map,
                "annotation_alignment_concurrency",
                defaults.annotation_alignment_concurrency,
            )?,
            review_concurrency: i64_field(&map, "review_concurrency", defaults.review_concurrency)?,
            review_output_retries,
            review_agent_loop: bool_field(&map, "review_agent_loop", defaults.review_agent_loop)?,
            review_agent_max_evidence_rounds,
            review_conflict_arbitration: bool_field(
                &map,
                "review_conflict_arbitration",
                defaults.review_conflict_arbitration,
            )?,
            review_fix_loop: bool_field(&map, "review_fix_loop", defaults.review_fix_loop)?,
            review_fix_max_rounds,
            review_clean_confirmations,
            review_autofix: bool_field(&map, "review_autofix", defaults.review_autofix)?,
            pdf_backend,
            babeldoc_bridge_url: string_field(&map, "babeldoc_bridge_url")?
                .unwrap_or(defaults.babeldoc_bridge_url),
            babeldoc_pages: string_field(&map, "babeldoc_pages")?,
            babeldoc_timeout: f64_field(&map, "babeldoc_timeout", defaults.babeldoc_timeout)?,
        })
    }
}

/// Output edition selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    /// Generate monolingual output.
    pub mono: bool,
    /// Generate bilingual output.
    pub bilingual: bool,
    /// Bilingual ordering.
    pub bilingual_order: BilingualOrder,
    /// Inherit book style for source blocks.
    pub bilingual_preserve_source_style: bool,
    /// Append the about page.
    pub about_page: bool,
    /// Normalize punctuation on export copies only.
    pub punctuation_normalize: bool,
}

/// Bilingual edition ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BilingualOrder {
    /// Translation first.
    TargetFirst,
    /// Source first.
    SourceFirst,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            mono: true,
            bilingual: false,
            bilingual_order: BilingualOrder::TargetFirst,
            bilingual_preserve_source_style: false,
            about_page: true,
            punctuation_normalize: true,
        }
    }
}

impl OutputConfig {
    fn from_value(raw: &serde_json::Value) -> Result<Self> {
        let map = object_or_null(raw, "output")?;
        check_extra(
            &map,
            &[
                "mono",
                "bilingual",
                "bilingual_order",
                "bilingual_preserve_source_style",
                "about_page",
                "punctuation_normalize",
            ],
            "output",
        )?;
        let bilingual_order = match map.get("bilingual_order") {
            None | Some(serde_json::Value::Null) => BilingualOrder::TargetFirst,
            Some(serde_json::Value::String(s)) => match s.as_str() {
                "target_first" => BilingualOrder::TargetFirst,
                "source_first" => BilingualOrder::SourceFirst,
                _ => {
                    return Err(Error::Config(format!(
                        "bilingual_order must be target_first or source_first, got {s:?}"
                    )))
                }
            },
            Some(_) => {
                return Err(Error::Config(
                    "bilingual_order must be a string".to_string(),
                ))
            }
        };
        Ok(Self {
            mono: bool_field(&map, "mono", true)?,
            bilingual: bool_field(&map, "bilingual", false)?,
            bilingual_order,
            bilingual_preserve_source_style: bool_field(
                &map,
                "bilingual_preserve_source_style",
                false,
            )?,
            about_page: bool_field(&map, "about_page", true)?,
            punctuation_normalize: bool_field(&map, "punctuation_normalize", true)?,
        })
    }
}

/// Provider connection envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Provider kind (deepseek, openai-compatible, ...).
    pub kind: String,
    /// Optional endpoint override.
    pub base_url: Option<String>,
    /// Environment variable holding the API key.
    pub api_key_env: Option<String>,
    /// Request timeout in seconds.
    pub timeout: f64,
    /// Connect timeout in seconds.
    pub connect_timeout: f64,
    /// Extra attempts after the first.
    pub max_retries: i64,
    /// Optional connection concurrency cap.
    pub max_concurrency: Option<i64>,
    /// Optional quota group name.
    pub quota_group: Option<String>,
    /// Adapter-owned connection options (e.g. `reasoning_style`); validated
    /// by the Slice 3 provider adapters, never silently accepted here.
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ProviderConfig {
    fn from_value(name: &str, raw: &serde_json::Value) -> Result<Self> {
        let map = object_or_null(raw, &format!("llm.providers.{name}"))?;
        let kind = string_field(&map, "kind")?
            .ok_or_else(|| Error::Config(format!("llm.providers.{name}.kind: field required")))?;
        let base_url = string_field(&map, "base_url")?;
        if let Some(url) = &base_url {
            if !base_url_valid(url) {
                return Err(Error::Config(format!(
                    "llm.providers.{name}.base_url: must be an absolute HTTP(S) endpoint \
                     without credentials, query or fragment"
                )));
            }
        }
        let api_key_env = string_field(&map, "api_key_env")?;
        if let Some(env) = &api_key_env {
            if !env_name_valid(env) {
                return Err(Error::Config(format!(
                    "llm.providers.{name}.api_key_env: must be an environment variable name"
                )));
            }
        }
        let timeout = f64_field(&map, "timeout", 600.0)?;
        if timeout <= 0.0 || !timeout.is_finite() {
            return Err(Error::Config(format!(
                "llm.providers.{name}.timeout: must be positive and finite"
            )));
        }
        let connect_timeout = f64_field(&map, "connect_timeout", 10.0)?;
        if connect_timeout <= 0.0 || !connect_timeout.is_finite() {
            return Err(Error::Config(format!(
                "llm.providers.{name}.connect_timeout: must be positive and finite"
            )));
        }
        let max_retries = i64_field(&map, "max_retries", 4)?;
        if max_retries < 0 {
            return Err(Error::Config(format!(
                "llm.providers.{name}.max_retries: must be >= 0"
            )));
        }
        let max_concurrency = opt_i64_field(&map, "max_concurrency")?;
        if matches!(max_concurrency, Some(n) if n <= 0) {
            return Err(Error::Config(format!(
                "llm.providers.{name}.max_concurrency: must be positive"
            )));
        }
        let mut extra = map.clone();
        for key in [
            "kind",
            "base_url",
            "api_key_env",
            "timeout",
            "connect_timeout",
            "max_retries",
            "max_concurrency",
            "quota_group",
        ] {
            extra.remove(key);
        }
        Ok(Self {
            kind,
            base_url,
            api_key_env,
            timeout,
            connect_timeout,
            max_retries,
            max_concurrency,
            quota_group: string_field(&map, "quota_group")?,
            extra,
        })
    }
}

/// Reusable model request profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Connection name.
    pub provider: String,
    /// Model identifier.
    pub model: String,
    /// Optional output token cap.
    pub max_output_tokens: Option<i64>,
    /// Optional input token cap.
    pub max_input_tokens: Option<i64>,
    /// Adapter-owned request options; validated by provider adapters.
    #[serde(default)]
    pub options: serde_json::Map<String, serde_json::Value>,
}

impl ModelConfig {
    fn from_value(name: &str, raw: &serde_json::Value) -> Result<Self> {
        let map = object_or_null(raw, &format!("llm.models.{name}"))?;
        check_extra(
            &map,
            &[
                "provider",
                "model",
                "max_output_tokens",
                "max_input_tokens",
                "options",
            ],
            &format!("llm.models.{name}"),
        )?;
        let provider = string_field(&map, "provider")?.unwrap_or_default();
        let model = string_field(&map, "model")?.unwrap_or_default();
        if provider.trim().is_empty() || model.trim().is_empty() {
            return Err(Error::Config(format!(
                "llm.models.{name}: provider and model names cannot be blank"
            )));
        }
        let max_output_tokens = opt_i64_field(&map, "max_output_tokens")?;
        if matches!(max_output_tokens, Some(n) if n <= 0) {
            return Err(Error::Config(format!(
                "llm.models.{name}.max_output_tokens: must be positive"
            )));
        }
        let max_input_tokens = opt_i64_field(&map, "max_input_tokens")?;
        if matches!(max_input_tokens, Some(n) if n <= 0) {
            return Err(Error::Config(format!(
                "llm.models.{name}.max_input_tokens: must be positive"
            )));
        }
        let options = match map.get("options") {
            None | Some(serde_json::Value::Null) => serde_json::Map::new(),
            Some(serde_json::Value::Object(options)) => options.clone(),
            Some(_) => {
                return Err(Error::Config(format!(
                    "llm.models.{name}.options: must be a mapping"
                )));
            }
        };
        Ok(Self {
            provider,
            model,
            max_output_tokens,
            max_input_tokens,
            options,
        })
    }
}

/// Operation route: exactly one of model or tier, plus fallbacks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteConfig {
    /// Direct model profile.
    pub model: Option<String>,
    /// Product tier.
    pub tier: Option<String>,
    /// Explicit fallback profiles.
    #[serde(default)]
    pub fallbacks: Vec<String>,
}

impl RouteConfig {
    fn from_value(operation: &str, raw: &serde_json::Value) -> Result<Self> {
        let map = object_or_null(raw, &format!("llm.routes.{operation}"))?;
        check_extra(
            &map,
            &["model", "tier", "fallbacks"],
            &format!("llm.routes.{operation}"),
        )?;
        let model = string_field(&map, "model")?;
        let tier = string_field(&map, "tier")?;
        if model.is_none() == tier.is_none() {
            return Err(Error::Config(format!(
                "llm.routes.{operation}: requires exactly one of model or tier"
            )));
        }
        if let Some(tier) = &tier {
            if !TIERS.contains(&tier.as_str()) {
                return Err(Error::Config(format!(
                    "llm.routes.{operation}: unknown tier {tier:?}"
                )));
            }
        }
        let fallbacks = match map.get("fallbacks") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str().map(str::to_string).ok_or_else(|| {
                        Error::Config(format!("llm.routes.{operation}.fallbacks: must be strings"))
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            Some(_) => {
                return Err(Error::Config(format!(
                    "llm.routes.{operation}.fallbacks: must be a list"
                )))
            }
        };
        Ok(Self {
            model,
            tier,
            fallbacks,
        })
    }
}

/// Account-group rate limits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuotaConfig {
    /// Requests per minute.
    pub requests_per_minute: Option<i64>,
    /// Tokens per minute.
    pub tokens_per_minute: Option<i64>,
}

impl QuotaConfig {
    fn from_value(name: &str, raw: &serde_json::Value) -> Result<Self> {
        let map = object_or_null(raw, &format!("llm.quotas.{name}"))?;
        check_extra(
            &map,
            &["requests_per_minute", "tokens_per_minute"],
            &format!("llm.quotas.{name}"),
        )?;
        let requests_per_minute = opt_i64_field(&map, "requests_per_minute")?;
        let tokens_per_minute = opt_i64_field(&map, "tokens_per_minute")?;
        for (key, value) in [
            ("requests_per_minute", requests_per_minute),
            ("tokens_per_minute", tokens_per_minute),
        ] {
            if matches!(value, Some(n) if n <= 0) {
                return Err(Error::Config(format!(
                    "llm.quotas.{name}.{key}: must be positive"
                )));
            }
        }
        Ok(Self {
            requests_per_minute,
            tokens_per_minute,
        })
    }
}

/// Invocation budget limits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Max requests.
    pub max_requests: Option<i64>,
    /// Max tokens.
    pub max_tokens: Option<i64>,
    /// Deadline in seconds.
    pub deadline_seconds: Option<f64>,
}

impl BudgetConfig {
    fn from_value(raw: &serde_json::Value) -> Result<Self> {
        let map = object_or_null(raw, "llm.budget")?;
        check_extra(
            &map,
            &["max_requests", "max_tokens", "deadline_seconds"],
            "llm.budget",
        )?;
        let max_requests = opt_i64_field(&map, "max_requests")?;
        let max_tokens = opt_i64_field(&map, "max_tokens")?;
        let deadline_seconds = opt_f64_field(&map, "deadline_seconds")?;
        for (key, value) in [("max_requests", max_requests), ("max_tokens", max_tokens)] {
            if matches!(value, Some(n) if n <= 0) {
                return Err(Error::Config(format!("llm.budget.{key}: must be positive")));
            }
        }
        if matches!(deadline_seconds, Some(n) if n <= 0.0 || !n.is_finite()) {
            return Err(Error::Config(
                "llm.budget.deadline_seconds: must be positive and finite".to_string(),
            ));
        }
        Ok(Self {
            max_requests,
            max_tokens,
            deadline_seconds,
        })
    }
}

/// LLM routing graph.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Connections by name.
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Model profiles by name.
    pub models: BTreeMap<String, ModelConfig>,
    /// Tier to model profile.
    pub tiers: BTreeMap<String, String>,
    /// Operation routes.
    pub routes: BTreeMap<String, RouteConfig>,
    /// Quota groups.
    pub quotas: BTreeMap<String, QuotaConfig>,
    /// Invocation budget.
    pub budget: BudgetConfig,
}

impl LlmConfig {
    fn from_value(raw: &serde_json::Value) -> Result<Self> {
        let mut map = object_or_null(raw, "llm")?;
        check_extra(
            &map,
            &[
                "preset",
                "providers",
                "models",
                "tiers",
                "routes",
                "quotas",
                "budget",
            ],
            "llm",
        )?;
        if map.is_empty() {
            map.insert(
                "preset".to_string(),
                serde_json::Value::String("deepseek".to_string()),
            );
        }
        let preset = map
            .get("preset")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(preset) = preset.as_deref() {
            if !PRESET_KINDS.contains(&preset) {
                return Err(Error::Config(format!(
                    "llm.preset: unknown provider kind {preset:?}"
                )));
            }
            expand_preset(preset, &mut map);
        }
        let providers = mapping(&map, "providers", ProviderConfig::from_value)?;
        let models = mapping(&map, "models", ModelConfig::from_value)?;
        let tiers = string_mapping(&map, "tiers")?;
        let routes = mapping(&map, "routes", RouteConfig::from_value)?;
        let quotas = mapping(&map, "quotas", QuotaConfig::from_value)?;
        let budget =
            BudgetConfig::from_value(map.get("budget").unwrap_or(&serde_json::Value::Null))?;
        for (section, name) in providers
            .keys()
            .map(|n| ("providers", n))
            .chain(models.keys().map(|n| ("models", n)))
            .chain(quotas.keys().map(|n| ("quotas", n)))
        {
            if !section_id_valid(name) {
                return Err(Error::Config(format!("Invalid llm.{section} ID: {name}")));
            }
        }
        let mut tier_keys: Vec<&str> = tiers.keys().map(String::as_str).collect();
        tier_keys.sort_unstable();
        let mut want = TIERS;
        want.sort_unstable();
        if tier_keys != want {
            return Err(Error::Config(
                "llm.tiers must define exactly strong, cheap and fast".to_string(),
            ));
        }
        for (name, connection) in &providers {
            if let Some(group) = &connection.quota_group {
                if !quotas.contains_key(group) {
                    return Err(Error::Config(format!(
                        "llm.providers.{name}.quota_group: unknown quota group"
                    )));
                }
            }
        }
        for (name, model) in &models {
            if !providers.contains_key(&model.provider) {
                return Err(Error::Config(format!(
                    "llm.models.{name}.provider: unknown connection {:?}",
                    model.provider
                )));
            }
        }
        for (name, reference) in &tiers {
            if !models.contains_key(reference) {
                return Err(Error::Config(format!(
                    "llm.tiers.{name}: unknown model profile {reference:?}"
                )));
            }
        }
        for (operation, route) in &routes {
            let mut refs: Vec<&str> = route
                .model
                .as_deref()
                .into_iter()
                .chain(route.fallbacks.iter().map(String::as_str))
                .collect();
            for reference in refs.drain(..) {
                if !models.contains_key(reference) {
                    return Err(Error::Config(format!(
                        "llm.routes.{operation}.model: unknown model profile {reference:?}"
                    )));
                }
            }
            let primary = route.model.as_deref().or_else(|| {
                route
                    .tier
                    .as_deref()
                    .and_then(|t| tiers.get(t).map(String::as_str))
            });
            if let Some(primary) = primary {
                let mut seen = std::collections::HashSet::from([primary]);
                for fallback in &route.fallbacks {
                    if !seen.insert(fallback.as_str()) {
                        return Err(Error::Config(format!(
                            "llm.routes.{operation}: duplicate fallback model"
                        )));
                    }
                }
            }
        }
        Ok(Self {
            providers,
            models,
            tiers,
            routes,
            quotas,
            budget,
        })
    }
}

/// Merge preset defaults under user-supplied sections (oracle `expand_preset`).
fn expand_preset(preset: &str, map: &mut serde_json::Map<String, serde_json::Value>) {
    let model_name = match preset {
        "fake" => "fake",
        "gemini" => "gemini-3.6-flash",
        _ => "deepseek-flash",
    };
    let mut providers = serde_json::Map::new();
    providers.insert("default".to_string(), serde_json::json!({"kind": preset}));
    let mut models = serde_json::Map::new();
    for tier in TIERS {
        models.insert(
            format!("default_{tier}"),
            serde_json::json!({"provider": "default", "model": model_name}),
        );
    }
    let mut tiers = serde_json::Map::new();
    for tier in TIERS {
        tiers.insert(
            tier.to_string(),
            serde_json::Value::String(format!("default_{tier}")),
        );
    }
    for (section, defaults) in [
        ("providers", providers),
        ("models", models),
        ("tiers", tiers),
    ] {
        let supplied = map
            .get(section)
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let mut merged = defaults;
        merged.extend(supplied);
        map.insert(section.to_string(), serde_json::Value::Object(merged));
    }
}

fn mapping<T>(
    map: &serde_json::Map<String, serde_json::Value>,
    section: &str,
    build: impl Fn(&str, &serde_json::Value) -> Result<T>,
) -> Result<BTreeMap<String, T>> {
    let section_map = match map.get(section) {
        None | Some(serde_json::Value::Null) => return Ok(BTreeMap::new()),
        Some(serde_json::Value::Object(entries)) => entries,
        Some(_) => return Err(Error::Config(format!("llm.{section} must be a mapping"))),
    };
    section_map
        .iter()
        .map(|(name, raw)| build(name, raw).map(|value| (name.clone(), value)))
        .collect()
}

fn string_mapping(
    map: &serde_json::Map<String, serde_json::Value>,
    section: &str,
) -> Result<BTreeMap<String, String>> {
    let section_map = match map.get(section) {
        None | Some(serde_json::Value::Null) => return Ok(BTreeMap::new()),
        Some(serde_json::Value::Object(entries)) => entries,
        Some(_) => return Err(Error::Config(format!("llm.{section} must be a mapping"))),
    };
    section_map
        .iter()
        .map(|(name, raw)| {
            raw.as_str()
                .map(|s| (name.clone(), s.to_string()))
                .ok_or_else(|| Error::Config(format!("llm.{section}.{name} must be a string")))
        })
        .collect()
}
