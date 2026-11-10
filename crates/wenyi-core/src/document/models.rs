//! Parsed document models. Oracle: `ingest/models.py`.

use serde::{Deserialize, Serialize};

/// Plain-text segment kind.
pub const KIND_TEXT: &str = "text";
/// Heading segment kind.
pub const KIND_HEADING: &str = "heading";

/// Smallest stable translation unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    /// Position within the chapter (0-based, re-indexed after splits).
    pub index: usize,
    /// Immutable source text.
    pub source: String,
    /// `text` or `heading`.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Translation target (`None` = pending).
    #[serde(default)]
    pub target: Option<String>,
    /// Pre-polish target for comparisons.
    #[serde(default)]
    pub target_before_polish: Option<String>,
    /// Stable markup anchor (`tn{resource}_{seg}` for EPUB/HTML).
    #[serde(default)]
    pub anchor: Option<String>,
    /// Physical XHTML resource (EPUB only).
    #[serde(default)]
    pub resource_href: Option<String>,
    /// Continuation of a split paragraph (merged on export, no own anchor).
    #[serde(default)]
    pub cont: bool,
    /// Format-specific metadata (heading levels, inline maps, ...).
    #[serde(default)]
    pub meta: serde_json::Map<String, serde_json::Value>,
}

fn default_kind() -> String {
    KIND_TEXT.to_string()
}

impl Segment {
    /// Plain-text segment with defaults.
    pub fn text(index: usize, source: impl Into<String>) -> Self {
        Self {
            index,
            source: source.into(),
            kind: KIND_TEXT.to_string(),
            target: None,
            target_before_polish: None,
            anchor: None,
            resource_href: None,
            cont: false,
            meta: serde_json::Map::new(),
        }
    }

    /// Whether this segment carries translatable text (oracle `text_segments`).
    pub fn has_text(&self) -> bool {
        !self.source.trim().is_empty()
    }
}

/// Stable logical section within a book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    /// Position within the book (0-based).
    pub index: usize,
    /// Display title.
    #[serde(default)]
    pub title: String,
    /// All segments (including blank ones).
    #[serde(default)]
    pub segments: Vec<Segment>,
    /// Source href for EPUB/HTML chapters.
    #[serde(default)]
    pub href: Option<String>,
    /// Raw template for backfill-capable formats.
    #[serde(default)]
    pub template: Option<String>,
    /// Chapter metadata (heading levels, ...).
    #[serde(default)]
    pub meta: serde_json::Map<String, serde_json::Value>,
}

impl Chapter {
    /// Segments with non-blank source (oracle `text_segments`).
    pub fn text_segments(&self) -> impl Iterator<Item = &Segment> {
        self.segments.iter().filter(|s| s.has_text())
    }
}

/// Parsed book: chapters plus format metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// Book title.
    #[serde(default)]
    pub title: String,
    /// Source language code.
    pub source_lang: String,
    /// Target language code.
    pub target_lang: String,
    /// Format tag (`text`, `epub`, `html`, `srt`, ...).
    pub fmt: String,
    /// Absolute source path.
    #[serde(default)]
    pub source_path: String,
    /// Logical chapters in order.
    #[serde(default)]
    pub chapters: Vec<Chapter>,
    /// Format metadata (EPUB schema, OPF path, TOC, ...).
    #[serde(default)]
    pub meta: serde_json::Map<String, serde_json::Value>,
}
