//! Tolerant parsing of model JSON output. Oracle: `llm/json_parser.py`.
//!
//! Strict parse first; on failure apply the oracle's narrow boundary-only
//! repairs (one missing root delimiter, surplus closers, fenced block).
//! Anything needing content synthesis stays an error — no `json-repair`
//! equivalent exists in the dependency shortlist, and silently inventing
//! payload content would be worse than failing loudly.

use serde_json::Value;

/// Model output remains unusable JSON after local repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonParseError {
    /// Truncated raw input for diagnostics (no secrets: model output only).
    pub preview: String,
}

impl std::fmt::Display for JsonParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cannot parse JSON: {:?}", self.preview)
    }
}

impl std::error::Error for JsonParseError {}

/// Parsed JSON with repair metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonParseResult {
    /// Parsed value.
    pub value: Value,
    /// Whether repair ran.
    pub repaired: bool,
    /// Repair classification.
    pub repair_kind: RepairKind,
}

/// How (and whether) the input was repaired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairKind {
    /// Strict parse succeeded.
    None,
    /// Only an outer boundary changed (safe for complete payloads).
    BoundaryOnly,
    /// Content was synthesized (never produced here; oracle parity marker).
    Structural,
}

impl JsonParseResult {
    /// Whether parsing proved repair did not synthesize payload content.
    pub fn safe_for_complete_payload(&self) -> bool {
        matches!(
            self.repair_kind,
            RepairKind::None | RepairKind::BoundaryOnly
        )
    }
}

fn strict_value(candidate: &str) -> Option<Value> {
    serde_json::from_str(candidate).ok()
}

/// Narrow boundary repairs mirroring the oracle's accepted transformations.
fn boundary_repair(raw: &str) -> Option<Value> {
    // One missing root closing delimiter.
    for delimiter in ["}", "]"] {
        let candidate = format!("{raw}{delimiter}");
        if let Some(value) = strict_value(&candidate) {
            return Some(value);
        }
    }
    // Surplus closing delimiters after a complete value.
    let mut without_extra = raw;
    while without_extra.ends_with(['}', ']']) {
        without_extra = without_extra[..without_extra.len() - 1].trim_end();
        if let Some(value) = strict_value(without_extra) {
            return Some(value);
        }
    }
    // Strict JSON value inside a Markdown JSON fence.
    let lines: Vec<&str> = raw.lines().collect();
    if lines.len() >= 3
        && matches!(lines[0].trim().to_lowercase().as_str(), "```" | "```json")
        && lines[lines.len() - 1].trim() == "```"
    {
        let inner = lines[1..lines.len() - 1].join("\n");
        if let Some(value) = strict_value(inner.trim()) {
            return Some(value);
        }
    }
    None
}

/// Parse model JSON, classifying boundary-only repairs.
pub fn parse_json_result(text: &str) -> Result<JsonParseResult, JsonParseError> {
    let raw = text.trim();
    if let Some(value) = strict_value(raw) {
        return Ok(JsonParseResult {
            value,
            repaired: false,
            repair_kind: RepairKind::None,
        });
    }
    if let Some(value) = boundary_repair(raw) {
        return Ok(JsonParseResult {
            value,
            repaired: true,
            repair_kind: RepairKind::BoundaryOnly,
        });
    }
    Err(JsonParseError {
        preview: raw.chars().take(200).collect(),
    })
}

/// Parse model JSON, delegating syntax recovery to boundary repair.
pub fn parse_json_loose(text: &str) -> Result<Value, JsonParseError> {
    parse_json_result(text).map(|result| result.value)
}
