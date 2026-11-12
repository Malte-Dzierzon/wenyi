//! EPUB template assembly: backfill translations into rebuilt physical resources.
//!
//! Oracle: `assemble/epub_writer.py::_assemble_epub`,
//! `assemble/epub_resources.py`, `assemble/html_renderer.py`,
//! `assemble/html_inline.py`, `assemble/html_bilingual.py`,
//! `assemble/epub_navigation.py`, `assemble/epub_presentation.py`.
//!
//! Deterministic; the original EPUB stays authoritative for layout
//! (templates are re-annotated, never stored).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

use crate::document::anchors::resolve_epub_href;
use crate::document::annotate::{
    annotate_resource, strip_ruby_markers, ANNOTATION_ID_ATTR, ANNOTATION_META_KEY, INLINE_ID_ATTR,
    INLINE_META_KEY, LINE_WRAPPER_ATTR, SEGMENT_ID_ATTR,
};
use crate::document::epub::{decode_markup, find_opf_path, parse_opf, OpfPackage};
use crate::document::layout::fragment_anchor_map;
use crate::document::markup::{parse_html, Dom, Node, NodeId};
use crate::document::models::{Chapter, Document, Segment, KIND_HEADING};
use crate::{Error, Result};

/// Bilingual source block style element id (oracle `_BILINGUAL_STYLE_ID`).
pub const BILINGUAL_STYLE_ID: &str = "tn-bilingual-style";
/// Synthetic source-anchor prefix (oracle `_SOURCE_ANCHOR_PREFIX`).
pub const SOURCE_ANCHOR_PREFIX: &str = "tn-source-";
/// Horizontal-layout override style id (oracle `_HORIZONTAL_OVERRIDE_ID`).
pub const HORIZONTAL_OVERRIDE_ID: &str = "trans-novel-horizontal-override";

/// Bilingual source CSS (oracle `_BILINGUAL_CSS`).
pub const BILINGUAL_CSS: &str = ".tn-source {\n  font-size: 0.88em;\n  line-height: 1.55;\n  color: #6b6b6b;\n  background-color: #f4f3f0;\n  padding: 0.5em 0.8em;\n  border-radius: 5px;\n  margin: 0.2em 0 1em;\n}\n@media (prefers-color-scheme: dark) {\n  .tn-source {\n    color: #a8a8a8;\n    background-color: #2a2a2a;\n    box-shadow: inset 0 0 0 1px rgba(255,255,255,0.14);\n  }\n}\n";

/// Horizontal-layout override CSS (oracle `_rewrite_html_document`).
const HORIZONTAL_CSS: &str = "html, body { writing-mode: horizontal-tb !important; -epub-writing-mode: horizontal-tb !important; -webkit-writing-mode: horizontal-tb !important; direction: ltr !important; text-orientation: mixed !important; } .vrtl, .vertical, [class*=\"vrtl\"] { writing-mode: horizontal-tb !important; -epub-writing-mode: horizontal-tb !important; -webkit-writing-mode: horizontal-tb !important; direction: ltr !important; }";

/// EPUB assembly options (oracle `assemble(..., out_format="epub")` flags).
#[derive(Debug, Clone, Default)]
pub struct AssembleOptions {
    /// Include source text blocks alongside translations.
    pub bilingual: bool,
    /// `target_first` (default) or `source_first` placement of source blocks.
    pub order: String,
    /// Reuse original classes/styles for source blocks instead of muted CSS.
    pub preserve_source_style: bool,
    /// Keep Japanese ruby in source mirrors (None = policy default by source lang).
    pub preserve_source_ruby: Option<bool>,
}

impl AssembleOptions {
    /// Mono-target assembly with defaults.
    pub fn monolingual() -> Self {
        Self {
            bilingual: false,
            order: "target_first".to_string(),
            preserve_source_style: false,
            preserve_source_ruby: None,
        }
    }
}

/// Nonempty translation, falling back to source (oracle `_seg_text`).
pub fn seg_text(segment: &Segment) -> &str {
    match segment.target.as_deref() {
        Some(target) if !target.trim().is_empty() => target,
        _ => segment.source.as_str(),
    }
}

/// Bilingual source text, blank when identical to a source-fallback (oracle `_bilingual_source`).
pub fn bilingual_source(source: &str, target: &str) -> String {
    if source == target {
        return String::new();
    }
    let stripped = strip_ruby_markers(source);
    if stripped.trim().is_empty() || stripped == target {
        String::new()
    } else {
        stripped
    }
}

/// Suffixed export title (oracle `_export_book_title`).
pub fn export_book_title(title: &str, target_lang: &str, bilingual: bool) -> String {
    let base = {
        let trimmed = title.trim();
        if trimmed.is_empty() {
            "translated".to_string()
        } else {
            trimmed.to_string()
        }
    };
    let lang = {
        let normalized = target_lang.trim().replace('_', "-").to_lowercase();
        if normalized.is_empty() {
            "zh".to_string()
        } else {
            normalized
        }
    };
    let suffix = if bilingual {
        format!("-wenyi-{lang}-bi")
    } else {
        format!("-wenyi-{lang}")
    };
    if base.ends_with(&suffix) {
        base
    } else {
        format!("{base}{suffix}")
    }
}

/// EPUB metadata language tag (oracle `_epub_lang`).
///
/// The policy oracle resolves via language profiles; only `zh` carries an
/// override (`zh-Hans`, verified against the pinned oracle). Everything else
/// passes through unchanged.
pub fn epub_lang(lang: &str) -> &str {
    if lang.is_empty() || lang == "zh" {
        "zh-Hans"
    } else {
        lang
    }
}

/// Physical XHTML inventory from state (oracle `_epub_resource_specs`).
pub fn resource_specs(meta: &serde_json::Map<String, serde_json::Value>) -> Vec<(usize, String)> {
    let mut resources = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let Some(list) = meta.get("epub_resources").and_then(|v| v.as_array()) else {
        return resources;
    };
    for (fallback_index, raw) in list.iter().enumerate() {
        let Some(record) = raw.as_object() else {
            continue;
        };
        let Some(href) = record.get("href").and_then(|v| v.as_str()) else {
            continue;
        };
        if href.is_empty() || !seen.insert(href) {
            continue;
        }
        let index = record
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .map(|i| i as usize)
            .unwrap_or(fallback_index);
        resources.push((index, href.to_string()));
    }
    resources
}

/// Anchor-merged render maps (oracle `_segment_render_maps`).
#[allow(clippy::type_complexity)]
pub fn segment_render_maps(
    segments: &[&Segment],
) -> (
    Vec<(String, String)>,
    BTreeMap<String, String>,
    BTreeMap<String, String>,
    BTreeMap<String, serde_json::Map<String, serde_json::Value>>,
) {
    let mut by_anchor: Vec<(String, String)> = Vec::new();
    let mut positions: BTreeMap<String, usize> = BTreeMap::new();
    let mut src_by_anchor: BTreeMap<String, String> = BTreeMap::new();
    let mut kind_by_anchor: BTreeMap<String, String> = BTreeMap::new();
    let mut meta_by_anchor: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
        BTreeMap::new();
    let mut current: Option<String> = None;
    for segment in segments {
        if segment.cont {
            if let Some(anchor) = current.as_ref() {
                if let Some(pos) = positions.get(anchor) {
                    by_anchor[*pos].1.push_str(seg_text(segment));
                }
                src_by_anchor
                    .entry(anchor.clone())
                    .or_default()
                    .push_str(&segment.source);
            }
            continue;
        }
        let Some(anchor) = segment.anchor.as_deref() else {
            current = None;
            continue;
        };
        // Oracle overwrites duplicate anchors (dict assignment); keep position
        // of first occurrence, replace text.
        current = Some(anchor.to_string());
        if let Some(pos) = positions.get(anchor) {
            by_anchor[*pos].1 = seg_text(segment).to_string();
        } else {
            positions.insert(anchor.to_string(), by_anchor.len());
            by_anchor.push((anchor.to_string(), seg_text(segment).to_string()));
        }
        src_by_anchor.insert(anchor.to_string(), segment.source.clone());
        kind_by_anchor.insert(anchor.to_string(), segment.kind.clone());
        meta_by_anchor.insert(anchor.to_string(), segment.meta.clone());
    }
    (by_anchor, src_by_anchor, kind_by_anchor, meta_by_anchor)
}

/// Merge persisted placements with reparsed DOM metadata (oracle `_merge_epub_render_meta`).
pub fn merge_render_meta(
    stored: &serde_json::Map<String, serde_json::Value>,
    fresh: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut merged = stored.clone();
    for (key, value) in fresh {
        merged.insert(key.clone(), value.clone());
    }
    let stored_annotations = stored.get(ANNOTATION_META_KEY).and_then(|v| v.as_object());
    let fresh_annotations = fresh.get(ANNOTATION_META_KEY).and_then(|v| v.as_object());
    if stored_annotations.is_some() || fresh_annotations.is_some() {
        let mut annotations = stored_annotations.cloned().unwrap_or_default();
        if let Some(fresh) = fresh_annotations {
            for (key, value) in fresh {
                annotations.insert(key.clone(), value.clone());
            }
        }
        if let Some(stored) = stored_annotations {
            for key in ["target_digest", "placements"] {
                if let Some(value) = stored.get(key) {
                    annotations.insert(key.to_string(), value.clone());
                }
            }
        }
        merged.insert(
            ANNOTATION_META_KEY.to_string(),
            serde_json::Value::Object(annotations),
        );
    }
    merged
}

/// SHA-256 hex of a string (oracle `hashlib.sha256(text.encode()).hexdigest()`).
fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    crate::document::hash::hex_of(&Sha256::digest(text.as_bytes()))
}

/// Append text, converting newlines into `<br/>` (oracle `_append_text_with_breaks`).
fn append_text_with_breaks(dom: &mut Dom, parent: NodeId, text: &str) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    for (index, line) in lines.iter().enumerate() {
        if !line.is_empty() {
            dom.append(
                parent,
                Node::Text {
                    text: (*line).to_string(),
                },
            );
        }
        if index + 1 < lines.len() {
            dom.append(
                parent,
                Node::Element {
                    tag: "br".to_string(),
                    attrs: Vec::new(),
                    children: Vec::new(),
                },
            );
        }
    }
}

/// Remove temporary backfill attributes (oracle `_clean_annotation_attrs`).
fn clean_annotation_attrs(dom: &mut Dom, id: NodeId) {
    dom.remove_attr(id, ANNOTATION_ID_ATTR);
    let stale: Vec<NodeId> = dom.find_all_descendants(id, |d| dom.has_attr(d, ANNOTATION_ID_ATTR));
    for node in stale {
        dom.remove_attr(node, ANNOTATION_ID_ATTR);
    }
}

/// Extract footnote markers from a range link (oracle `_range_marker_nodes`).
fn range_marker_nodes(dom: &mut Dom, root: NodeId, marker_text: &str) -> Vec<NodeId> {
    if marker_text.is_empty() {
        return Vec::new();
    }
    let sup_subs: Vec<NodeId> =
        dom.find_all_descendants(root, |d| matches!(dom.tag(d), Some("sup" | "sub")));
    for node in sup_subs.into_iter().rev() {
        if dom.get_text(node, "", true) == marker_text {
            dom.detach(node);
            return vec![node];
        }
    }
    let all: Vec<NodeId> = dom.find_all_descendants(root, |d| dom.tag(d).is_some());
    for node in all.into_iter().rev() {
        if dom.get_text(node, "", true) == marker_text
            && !dom.children(node).iter().any(|c| dom.tag(*c).is_some())
        {
            dom.detach(node);
            return vec![node];
        }
    }
    Vec::new()
}

/// Paragraph-end fallback marker (oracle `_fallback_annotation_node`).
fn fallback_annotation_node(dom: &mut Dom, root: NodeId, mode: &str, marker_text: &str) {
    clean_annotation_attrs(dom, root);
    if mode != "range" {
        return;
    }
    let markers = range_marker_nodes(dom, root, marker_text);
    dom.clear(root);
    if markers.is_empty() {
        let text = if marker_text.is_empty() {
            "↩"
        } else {
            marker_text
        };
        dom.append(
            root,
            Node::Text {
                text: text.to_string(),
            },
        );
    } else {
        for marker in markers {
            dom.move_to(marker, root);
        }
    }
}

/// JSON int excluding bools (oracle `isinstance(x, int) and not isinstance(x, bool)`).
fn json_int(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// Extract annotation DOM into point, range and fallback groups (oracle `_annotation_restorations`).
#[allow(clippy::type_complexity)]
fn annotation_restorations(
    dom: &mut Dom,
    el: NodeId,
    text: &str,
    meta: &serde_json::Map<String, serde_json::Value>,
) -> (
    Vec<(usize, usize, NodeId)>,
    Vec<(usize, usize, usize, NodeId, Vec<NodeId>)>,
    Vec<NodeId>,
) {
    let empty = serde_json::Map::new();
    let annotations = meta
        .get(ANNOTATION_META_KEY)
        .and_then(|v| v.as_object())
        .unwrap_or(&empty);
    let items: Vec<&serde_json::Map<String, serde_json::Value>> = annotations
        .get("items")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(|v| v.as_object()).collect())
        .unwrap_or_default();
    let mut placement_by_id: BTreeMap<&str, &serde_json::Map<String, serde_json::Value>> =
        BTreeMap::new();
    if let Some(placements) = annotations.get("placements").and_then(|v| v.as_array()) {
        for placement in placements.iter().filter_map(|v| v.as_object()) {
            if let Some(id) = placement.get("id").and_then(|v| v.as_str()) {
                placement_by_id.insert(id, placement);
            }
        }
    }
    let text_len = text.chars().count();
    let digest_matches = annotations
        .get("target_digest")
        .and_then(|v| v.as_str())
        .is_some_and(|d| d == sha256_hex(text));
    let mut points: Vec<(usize, usize, NodeId)> = Vec::new();
    let mut ranges: Vec<(usize, usize, usize, NodeId, Vec<NodeId>)> = Vec::new();
    let mut fallbacks: Vec<NodeId> = Vec::new();
    let mut pending: Vec<(usize, usize, usize, NodeId, Vec<NodeId>, String)> = Vec::new();
    // Oracle iterates `enumerate(items)` over raw list including non-dicts; our
    // filtered vec can only differ on malformed meta (filtered items skipped).
    for (order, item) in items.iter().enumerate() {
        let (Some(annotation_id), Some(mode)) = (
            item.get("id").and_then(|v| v.as_str()),
            item.get("mode").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        if mode != "point" && mode != "range" {
            continue;
        }
        let Some(root) = dom.find_descendant(el, |d| {
            dom.attr(d, ANNOTATION_ID_ATTR) == Some(annotation_id)
        }) else {
            continue;
        };
        dom.detach(root);
        clean_annotation_attrs(dom, root);
        let marker_text = item
            .get("marker_text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let placement = placement_by_id.get(annotation_id).copied();
        let start = placement
            .and_then(|p| p.get("target_start"))
            .and_then(json_int);
        let end = placement
            .and_then(|p| p.get("target_end"))
            .and_then(json_int);
        let status = placement
            .and_then(|p| p.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let method = placement
            .and_then(|p| p.get("method"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let rejected = |s: &str| {
            matches!(
                s.to_lowercase().as_str(),
                "fallback" | "failed" | "invalid" | "missing" | "stale"
            )
        };
        let usable = digest_matches
            && matches!((start, end), (Some(s), Some(e)) if s >= 0 && s <= e && (e as usize) <= text_len)
            && !rejected(status)
            && !rejected(method);
        if !usable {
            let source_start = item.get("source_start").and_then(json_int);
            let source_end = item.get("source_end").and_then(json_int);
            let source_length = annotations.get("source_length").and_then(json_int);
            let covers_whole = mode == "range"
                && marker_text.is_empty()
                && items.len() == 1
                && matches!((source_start, source_end, source_length), (Some(0), Some(e), Some(l)) if e == l && l > 0);
            if covers_whole {
                pending.push((0, text_len, order, root, Vec::new(), marker_text));
                continue;
            }
            fallback_annotation_node(dom, root, mode, &marker_text);
            fallbacks.push(root);
            continue;
        }
        let (Some(start), Some(end)) = (start, end) else {
            continue;
        };
        let (start, end) = (start as usize, end as usize);
        if mode == "point" && start == end {
            points.push((start, order, root));
            continue;
        }
        if mode == "range" && start < end {
            let markers = range_marker_nodes(dom, root, &marker_text);
            pending.push((start, end, order, root, markers, marker_text));
            continue;
        }
        fallback_annotation_node(dom, root, mode, &marker_text);
        fallbacks.push(root);
    }
    pending.sort();
    let mut last_end: i64 = -1;
    for (start, end, order, root, markers, marker_text) in pending {
        if (start as i64) < last_end {
            dom.clear(root);
            if markers.is_empty() {
                let text = if marker_text.is_empty() {
                    "↩"
                } else {
                    marker_text.as_str()
                };
                dom.append(
                    root,
                    Node::Text {
                        text: text.to_string(),
                    },
                );
            } else {
                for marker in markers {
                    dom.move_to(marker, root);
                }
            }
            fallbacks.push(root);
            continue;
        }
        ranges.push((start, end, order, root, markers));
        last_end = end as i64;
    }
    let mut safe_points = Vec::new();
    for (offset, order, root) in points {
        if ranges.iter().any(|(s, e, ..)| *s < offset && offset < *e) {
            fallbacks.push(root);
        } else {
            safe_points.push((offset, order, root));
        }
    }
    (safe_points, ranges, fallbacks)
}

/// Slice by char offsets (offsets validated against char counts upstream).
fn char_slice(text: &str, start: usize, end: usize) -> &str {
    let start_byte = text
        .char_indices()
        .nth(start)
        .map(|(b, _)| b)
        .unwrap_or(text.len());
    let end_byte = text
        .char_indices()
        .nth(end)
        .map(|(b, _)| b)
        .unwrap_or(text.len());
    &text[start_byte..end_byte.max(start_byte)]
}

/// Backfill inline nodes, points, ranges at target offsets (oracle `_render_text_with_nodes`).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn render_text_with_nodes(
    dom: &mut Dom,
    el: NodeId,
    text: &str,
    nodes: &[(usize, usize, NodeId)],
    ranges: &[(usize, usize, usize, NodeId, Vec<NodeId>)],
    fallbacks: &[NodeId],
) {
    // Hoist processing instructions before the block (oracle parity).
    let pis: Vec<NodeId> = dom.find_all_descendants(el, |d| dom.is_pi(d));
    for pi in pis {
        dom.move_before(pi, el);
    }
    let mut ordered: Vec<(usize, usize, NodeId)> = nodes.to_vec();
    ordered.sort();
    let mut node_index = 0usize;
    // Byte-level rendering: drain ordered nodes up to each boundary.
    let mut sorted_ranges: Vec<&(usize, usize, usize, NodeId, Vec<NodeId>)> =
        ranges.iter().collect();
    sorted_ranges.sort();
    dom.clear(el);
    let mut cursor = 0usize;
    for (start, end, _order, root, markers) in sorted_ranges {
        let (_, next_index) =
            append_until(dom, el, text, &ordered, node_index, cursor, *start, true);
        node_index = next_index;
        dom.clear(*root);
        let (_, next_index) =
            append_until(dom, *root, text, &ordered, node_index, *start, *end, false);
        node_index = next_index;
        for marker in markers {
            dom.move_to(*marker, *root);
        }
        dom.move_to(*root, el);
        cursor = *end;
    }
    let text_len = text.chars().count();
    let _ = append_until(dom, el, text, &ordered, node_index, cursor, text_len, true);
    for (index, fallback) in fallbacks.iter().enumerate() {
        if index > 0 {
            dom.append(
                el,
                Node::Text {
                    text: "、".to_string(),
                },
            );
        }
        dom.move_to(*fallback, el);
    }
}

/// Drain ordered inline nodes with text up to `end` (oracle `append_until`).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn append_until(
    dom: &mut Dom,
    parent: NodeId,
    text: &str,
    ordered: &[(usize, usize, NodeId)],
    mut node_index: usize,
    start: usize,
    end: usize,
    include_end: bool,
) -> (usize, usize) {
    let text_len = text.chars().count();
    let mut cursor = start.min(text_len).min(end);
    let end = end.min(text_len);
    while node_index < ordered.len()
        && (ordered[node_index].0 < end || (include_end && ordered[node_index].0 == end))
    {
        let (mut offset, _order, node) = ordered[node_index];
        node_index += 1;
        offset = offset.max(cursor).min(end);
        if offset > cursor {
            append_text_with_breaks(dom, parent, char_slice(text, cursor, offset));
        }
        dom.move_to(node, parent);
        cursor = offset;
    }
    if cursor < end {
        append_text_with_breaks(dom, parent, char_slice(text, cursor, end));
    }
    (cursor, node_index)
}

/// Replace block content, restoring inline/annotation nodes (oracle `_replace_block_content`).
fn replace_block_content(
    dom: &mut Dom,
    el: NodeId,
    text: &str,
    meta: &serde_json::Map<String, serde_json::Value>,
) {
    let text_len = text.chars().count();
    let mut self_markers: Vec<NodeId> = Vec::new();
    if let Some(self_id) = dom.attr(el, ANNOTATION_ID_ATTR).map(str::to_string) {
        let empty = serde_json::Map::new();
        let annotations = meta
            .get(ANNOTATION_META_KEY)
            .and_then(|v| v.as_object())
            .unwrap_or(&empty);
        let marker_text = annotations
            .get("items")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_object())
            .find(|item| item.get("id").and_then(|v| v.as_str()) == Some(self_id.as_str()))
            .and_then(|item| item.get("marker_text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        self_markers = range_marker_nodes(dom, el, marker_text);
        dom.remove_attr(el, ANNOTATION_ID_ATTR);
    }
    let empty = serde_json::Map::new();
    let inline = meta
        .get(INLINE_META_KEY)
        .and_then(|v| v.as_object())
        .unwrap_or(&empty);
    let nodes: Vec<&serde_json::Map<String, serde_json::Value>> = inline
        .get("nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| nodes.iter().filter_map(|v| v.as_object()).collect())
        .unwrap_or_default();
    let source_length = inline
        .get("source_length")
        .and_then(json_int)
        .unwrap_or(0)
        .max(0) as usize;
    let mut restored: Vec<(usize, usize, NodeId)> = Vec::new();
    for (order, record) in nodes.iter().enumerate() {
        let (Some(inline_id), Some(offset)) = (
            record.get("id").and_then(|v| v.as_str()),
            record.get("offset").and_then(json_int),
        ) else {
            continue;
        };
        let Some(node) =
            dom.find_descendant(el, |d| dom.attr(d, INLINE_ID_ATTR) == Some(inline_id))
        else {
            continue;
        };
        dom.detach(node);
        dom.remove_attr(node, INLINE_ID_ATTR);
        let target_offset = if offset <= 0 {
            0
        } else if source_length == 0 || (offset as usize) >= source_length {
            text_len
        } else {
            // Oracle `round(offset * len / source_length)` = banker's rounding.
            bankers_round(offset as f64 * text_len as f64 / source_length as f64)
                .min(text_len as i64)
                .max(0) as usize
        };
        restored.push((target_offset, nodes.len() + order, node));
    }
    let (points, ranges, fallbacks) = annotation_restorations(dom, el, text, meta);
    let mut all_nodes = points;
    all_nodes.extend(restored);
    render_text_with_nodes(dom, el, text, &all_nodes, &ranges, &fallbacks);
    for marker in self_markers {
        dom.move_to(marker, el);
    }
}

/// Python `round()` semantics (round-half-to-even) for offset scaling.
fn bankers_round(value: f64) -> i64 {
    let floor = value.floor();
    let diff = value - floor;
    if diff < 0.5 {
        floor as i64
    } else if diff > 0.5 {
        (floor + 1.0) as i64
    } else if (floor as i64) % 2 == 0 {
        floor as i64
    } else {
        (floor + 1.0) as i64
    }
}

/// Whether source lang keeps ruby in bilingual mirrors (oracle policy default).
///
/// The pinned oracle enables `markup.japanese_ruby` only for `ja` sources
/// (verified via `resolve_policy`); explicit `preserve_source_ruby` overrides.
fn default_keep_ruby(source_lang: &str) -> bool {
    source_lang == "ja"
}

/// Sanitized annotation/ruby markup for a bilingual source block (oracle `_bilingual_source_markup`).
fn bilingual_source_markup(
    dom: &Dom,
    el: NodeId,
    source_lang: &str,
    resource_href: &str,
    source_link_targets: &BTreeMap<(String, String), String>,
    preserve_source_ruby: Option<bool>,
) -> String {
    let keep_ruby = preserve_source_ruby.unwrap_or_else(|| default_keep_ruby(source_lang));
    let has_annotation = dom.has_attr(el, ANNOTATION_ID_ATTR)
        || dom
            .find_descendant(el, |d| dom.has_attr(d, ANNOTATION_ID_ATTR))
            .is_some();
    if !has_annotation
        && (!keep_ruby
            || dom
                .find_descendant(el, |d| dom.tag(d) == Some("ruby"))
                .is_none())
    {
        return String::new();
    }
    // Clone the element subtree into a scratch DOM (oracle re-parses `str(element)`).
    let mut scratch = Dom::new();
    let root = clone_subtree(dom, el, &mut scratch, 0);
    let root_is_annotation = scratch.has_attr(root, ANNOTATION_ID_ATTR);
    let mut retained: BTreeSet<NodeId> = BTreeSet::new();
    for annotation in
        scratch.find_all_descendants(root, |d| scratch.has_attr(d, ANNOTATION_ID_ATTR))
    {
        retained.insert(annotation);
        retained.extend(scratch.descendants(annotation));
    }
    if root_is_annotation {
        retained.insert(root);
        retained.extend(scratch.descendants(root));
    }
    if keep_ruby {
        for ruby in scratch.find_all_descendants(root, |d| scratch.tag(d) == Some("ruby")) {
            retained.insert(ruby);
            retained.extend(scratch.descendants(ruby));
        }
    }
    let comments: Vec<NodeId> = scratch
        .descendants(root)
        .into_iter()
        .filter(|d| matches!(scratch.nodes[*d], Node::Comment { .. }))
        .collect();
    for comment in comments {
        scratch.remove_subtree(comment);
    }
    const FLATTEN_TAGS: [&str; 13] = [
        "audio", "canvas", "embed", "hr", "iframe", "img", "math", "object", "script", "source",
        "style", "svg", "video",
    ];
    let flatten: Vec<NodeId> = scratch.find_all_descendants(root, |d| {
        scratch.tag(d).is_some_and(|t| FLATTEN_TAGS.contains(&t))
    });
    for node in flatten {
        scratch.remove_subtree(node);
        retained.remove(&node);
    }
    if !keep_ruby {
        let readings: Vec<NodeId> =
            scratch.find_all_descendants(root, |d| matches!(scratch.tag(d), Some("rt" | "rp")));
        for node in readings {
            scratch.remove_subtree(node);
            retained.remove(&node);
        }
    }
    // Unwrap non-retained tags; strip temp attrs from retained ones.
    for node in scratch.find_all_descendants(root, |d| scratch.tag(d).is_some()) {
        if retained.contains(&node) {
            for attr in [
                "id",
                "name",
                SEGMENT_ID_ATTR,
                INLINE_ID_ATTR,
                ANNOTATION_ID_ATTR,
                LINE_WRAPPER_ATTR,
            ] {
                scratch.remove_attr(node, attr);
            }
        } else {
            scratch.unwrap(node);
        }
    }
    // Rewrite source-mirror links to synthetic anchors where mapped.
    let mut links: Vec<NodeId> = Vec::new();
    if scratch.tag(root) == Some("a") {
        links.push(root);
    }
    links.extend(scratch.find_all_descendants(root, |d| {
        scratch.tag(d) == Some("a") && scratch.attr(d, "href").is_some()
    }));
    for link in links {
        let Some(raw_href) = scratch.attr(link, "href").map(str::to_string) else {
            continue;
        };
        let resolved = resolve_epub_href(resource_href, &raw_href);
        let Some(target) =
            source_link_targets.get(&(resolved.resource_href.clone(), resolved.fragment.clone()))
        else {
            continue;
        };
        if resolved.external || resolved.fragment.is_empty() {
            continue;
        };
        if let Some((path_and_query, _)) = raw_href.split_once('#') {
            scratch.set_attr(link, "href", &format!("{path_and_query}#{target}"));
        }
    }
    if root_is_annotation {
        scratch.serialize_node_str(root)
    } else {
        scratch.serialize_contents(root)
    }
}

/// Deep-copy a subtree into another DOM (used for bilingual source mirrors).
fn clone_subtree(dom: &Dom, id: NodeId, scratch: &mut Dom, parent: NodeId) -> NodeId {
    let node = match &dom.nodes[id] {
        Node::Root { .. } => Node::Root {
            children: Vec::new(),
        },
        Node::Element { tag, attrs, .. } => Node::Element {
            tag: tag.clone(),
            attrs: attrs.clone(),
            children: Vec::new(),
        },
        Node::Text { text } => Node::Text { text: text.clone() },
        Node::Comment { body } => Node::Comment { body: body.clone() },
        Node::Pi { body } => Node::Pi { body: body.clone() },
        Node::Doctype { body } => Node::Doctype { body: body.clone() },
    };
    let fresh = scratch.append(parent, node);
    for child in dom.children(id) {
        clone_subtree(dom, child, scratch, fresh);
    }
    fresh
}

/// Append plain or markup source into a source block (oracle `_append_source`).
fn append_source(dom: &mut Dom, element: NodeId, source: &str, markup: &str) {
    if markup.is_empty() {
        dom.append(
            element,
            Node::Text {
                text: source.to_string(),
            },
        );
        return;
    }
    let fragment = parse_html(markup);
    for child in fragment.children(0) {
        clone_subtree(&fragment, child, dom, element);
    }
}

/// Occupied id/name set + `data-tn-id` index (oracle `_index_soup_ids`).
fn index_dom_ids(dom: &Dom) -> (BTreeSet<String>, BTreeMap<String, NodeId>) {
    let mut occupied = BTreeSet::new();
    let mut index = BTreeMap::new();
    for node in dom.descendants(0) {
        if dom.tag(node).is_none() {
            continue;
        }
        for attr in ["id", "name"] {
            if let Some(value) = dom.attr(node, attr) {
                if !value.is_empty() {
                    occupied.insert(value.to_string());
                }
            }
        }
        if let Some(tn_id) = dom.attr(node, SEGMENT_ID_ATTR) {
            if !tn_id.is_empty() {
                index.insert(tn_id.to_string(), node);
            }
        }
    }
    (occupied, index)
}

/// Collision-free synthetic source ids (oracle `_build_source_anchor_ids`).
#[allow(clippy::type_complexity)]
fn build_source_anchor_ids(
    by_anchor: &[(String, String)],
    src_by_anchor: &BTreeMap<String, String>,
    kind_by_anchor: &BTreeMap<String, String>,
    tn_id_index: &BTreeMap<String, NodeId>,
    occupied: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    let mut occupied = occupied.clone();
    let mut assigned = BTreeMap::new();
    for (anchor, target) in by_anchor {
        if kind_by_anchor
            .get(anchor)
            .is_some_and(|k| k == KIND_HEADING)
        {
            continue;
        }
        let source = bilingual_source(
            src_by_anchor.get(anchor).map(String::as_str).unwrap_or(""),
            target,
        );
        if source.is_empty() || !tn_id_index.contains_key(anchor) {
            continue;
        }
        let base = format!("{SOURCE_ANCHOR_PREFIX}{anchor}");
        let mut candidate = base.clone();
        let mut suffix = 2;
        while occupied.contains(&candidate) {
            candidate = format!("{base}-{suffix}");
            suffix += 1;
        }
        assigned.insert(anchor.clone(), candidate.clone());
        occupied.insert(candidate);
    }
    assigned
}

/// Backfill one physical resource template (oracle `_render_segments_html`).
#[allow(clippy::too_many_arguments)]
pub fn render_segments_html(
    template: &str,
    segments: &[&Segment],
    render_meta_by_anchor: Option<&BTreeMap<String, serde_json::Map<String, serde_json::Value>>>,
    bilingual: bool,
    order: &str,
    preserve_source_style: bool,
    source_lang: &str,
    preserve_source_ruby: Option<bool>,
    resource_href: &str,
    source_ids_by_anchor: Option<BTreeMap<String, String>>,
    source_link_targets: Option<BTreeMap<(String, String), String>>,
) -> String {
    let mut dom = parse_html(template);
    let (by_anchor, src_by_anchor, kind_by_anchor, stored_meta) = segment_render_maps(segments);
    let (occupied, tn_id_index) = index_dom_ids(&dom);
    let source_ids = source_ids_by_anchor.unwrap_or_else(|| {
        if bilingual {
            build_source_anchor_ids(
                &by_anchor,
                &src_by_anchor,
                &kind_by_anchor,
                &tn_id_index,
                &occupied,
            )
        } else {
            BTreeMap::new()
        }
    });
    let link_targets = source_link_targets.unwrap_or_else(|| {
        if !bilingual {
            return BTreeMap::new();
        }
        let mut targets = BTreeMap::new();
        for (fragment, anchor) in fragment_anchor_map(template) {
            if fragment.is_empty() {
                continue;
            }
            if let Some(anchor) = anchor {
                if let Some(source_id) = source_ids.get(&anchor) {
                    targets.insert((resource_href.to_string(), fragment), source_id.clone());
                }
            }
        }
        targets
    });
    for (anchor, text) in &by_anchor {
        let Some(el) = tn_id_index.get(anchor).copied() else {
            continue;
        };
        // The index may point at a node detached by an earlier iteration's
        // unwrap; skip anything no longer attached to this document.
        if dom.parents[el] >= dom.nodes.len() {
            continue;
        }
        let empty_source = String::new();
        let stored_source = src_by_anchor.get(anchor).unwrap_or(&empty_source);
        let src = if bilingual
            && kind_by_anchor
                .get(anchor)
                .is_some_and(|k| k != KIND_HEADING)
        {
            bilingual_source(stored_source, text)
        } else {
            String::new()
        };
        let source_markup = if src.is_empty() {
            String::new()
        } else {
            bilingual_source_markup(
                &dom,
                el,
                source_lang,
                resource_href,
                &link_targets,
                preserve_source_ruby,
            )
        };
        let line_wrapper = dom.has_attr(el, LINE_WRAPPER_ATTR);
        let empty_meta = serde_json::Map::new();
        let stored_meta = stored_meta.get(anchor).unwrap_or(&empty_meta);
        let fresh_meta = render_meta_by_anchor
            .and_then(|m| m.get(anchor))
            .unwrap_or(&empty_meta);
        let render_meta = merge_render_meta(stored_meta, fresh_meta);
        if text != stored_source {
            replace_block_content(&mut dom, el, text, &render_meta);
        }
        dom.remove_attr(el, SEGMENT_ID_ATTR);
        if src.is_empty() {
            continue;
        }
        let nested_source = matches!(dom.tag(el), Some("li" | "blockquote"));
        let src_tag = if line_wrapper {
            "span"
        } else if nested_source {
            "div"
        } else {
            "p"
        };
        let src_el = dom.append(
            0,
            Node::Element {
                tag: src_tag.to_string(),
                attrs: Vec::new(),
                children: Vec::new(),
            },
        );
        if preserve_source_style {
            let mut classes: Vec<String> = dom
                .attr(el, "class")
                .map(|c| c.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default();
            if !classes.iter().any(|c| c == "tn-source") {
                classes.push("tn-source".to_string());
            }
            dom.set_attr(src_el, "class", &classes.join(" "));
            if let Some(style) = dom.attr(el, "style").map(str::to_string) {
                dom.set_attr(src_el, "style", &style);
            }
        } else {
            dom.set_attr(
                src_el,
                "class",
                "tn-source ibooks-dark-theme-use-custom-text-color",
            );
        }
        if let Some(source_id) = source_ids.get(anchor) {
            dom.set_attr(src_el, "id", source_id);
        }
        append_source(&mut dom, src_el, &src, &source_markup);
        if line_wrapper && order == "source_first" {
            dom.move_before(src_el, el);
            let br = Node::Element {
                tag: "br".to_string(),
                attrs: Vec::new(),
                children: Vec::new(),
            };
            dom.insert_after(src_el, br);
        } else if line_wrapper {
            dom.move_after(src_el, el);
            let br = dom.append(
                0,
                Node::Element {
                    tag: "br".to_string(),
                    attrs: Vec::new(),
                    children: Vec::new(),
                },
            );
            dom.move_before(br, src_el);
        } else if nested_source && order == "source_first" {
            // Oracle `el.insert(0, src_el)`: prepend inside the container.
            let first = dom.children(el).into_iter().next();
            match first {
                Some(first) => dom.move_before(src_el, first),
                None => dom.move_to(src_el, el),
            }
        } else if nested_source {
            dom.move_to(src_el, el);
        } else if order == "source_first" {
            dom.move_before(src_el, el);
        } else {
            dom.move_after(src_el, el);
        }
    }
    // Unwrap temporary line-break wrappers for a clean DOM.
    for wrapper in dom.find_all_descendants(0, |d| dom.has_attr(d, LINE_WRAPPER_ATTR)) {
        dom.remove_attr(wrapper, LINE_WRAPPER_ATTR);
        dom.unwrap(wrapper);
    }
    for node in dom.find_all_descendants(0, |d| dom.has_attr(d, ANNOTATION_ID_ATTR)) {
        dom.remove_attr(node, ANNOTATION_ID_ATTR);
    }
    for node in dom.find_all_descendants(0, |d| dom.has_attr(d, INLINE_ID_ATTR)) {
        dom.remove_attr(node, INLINE_ID_ATTR);
    }
    dom.serialize()
}

/// Prepared per-resource render inputs: (segments, template, fresh meta by anchor).
type PreparedResources<'a> = BTreeMap<
    String,
    (
        Vec<&'a Segment>,
        String,
        BTreeMap<String, serde_json::Map<String, serde_json::Value>>,
    ),
>;

/// Rebuild + render each physical XHTML once (oracle `_render_epub_resources`).
pub fn render_epub_resources<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    chapters: &[Chapter],
    meta: &serde_json::Map<String, serde_json::Value>,
    options: &AssembleOptions,
    book_title: &str,
    source_lang: &str,
) -> Result<BTreeMap<String, String>> {
    let resources = resource_specs(meta);
    let mut grouped: BTreeMap<&str, Vec<&Segment>> = BTreeMap::new();
    for chapter in chapters {
        for segment in &chapter.segments {
            if let Some(href) = segment.resource_href.as_deref() {
                grouped.entry(href).or_default().push(segment);
            }
        }
    }
    if resources.is_empty() || grouped.is_empty() {
        return Ok(BTreeMap::new());
    }
    let declared: BTreeSet<&str> = resources.iter().map(|(_, href)| href.as_str()).collect();
    let undeclared: Vec<&&str> = grouped
        .keys()
        .filter(|href| !declared.contains(**href))
        .collect();
    if !undeclared.is_empty() {
        let preview = undeclared
            .iter()
            .take(3)
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::Store(format!(
            "EPUB state refers to undeclared body resources: {preview}"
        )));
    }
    let names: BTreeSet<String> = archive
        .file_names()
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| Error::Store(format!("Invalid EPUB zip: {e}")))?
        .into_iter()
        .map(|s| s.into_owned())
        .collect();
    let toc_paths: BTreeSet<&str> = meta
        .get("toc_paths")
        .and_then(|v| v.as_array())
        .map(|paths| paths.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let mut prepared: PreparedResources<'_> = BTreeMap::new();
    for (resource_index, href) in &resources {
        let Some(segments) = grouped.get(href.as_str()) else {
            continue;
        };
        if !names.contains(href) {
            return Err(Error::Store(format!(
                "EPUB body resource not found: {href}"
            )));
        }
        let mut entry = archive
            .by_name(href)
            .map_err(|e| Error::Store(format!("EPUB body resource not found: {href}: {e}")))?;
        let mut data = Vec::new();
        use std::io::Read;
        entry.read_to_end(&mut data).map_err(|e| Error::Io {
            path: Path::new(href).to_path_buf(),
            message: e.to_string(),
        })?;
        drop(entry);
        let html = decode_markup(&data);
        let (_title, annotated, template) = annotate_resource(
            &html,
            *resource_index,
            href,
            book_title,
            toc_paths.contains(href.as_str()),
        );
        let available: BTreeSet<&str> = annotated
            .iter()
            .filter_map(|s| s.anchor.as_deref())
            .collect();
        let required: BTreeSet<&str> = segments
            .iter()
            .filter(|s| !s.cont)
            .filter_map(|s| s.anchor.as_deref())
            .collect();
        let missing: Vec<&&str> = required.difference(&available).collect();
        if !missing.is_empty() {
            let preview = missing
                .iter()
                .take(3)
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(Error::Store(format!(
                "EPUB body does not match translation state: {href} lacks backfill anchors {preview}"
            )));
        }
        let fresh_by_anchor: BTreeMap<&str, &Segment> = annotated
            .iter()
            .filter_map(|s| s.anchor.as_deref().map(|a| (a, s)))
            .collect();
        let mut stored_sources: BTreeMap<&str, String> = BTreeMap::new();
        let mut current: Option<&str> = None;
        for segment in segments {
            if segment.cont {
                if let Some(anchor) = current {
                    stored_sources
                        .entry(anchor)
                        .or_default()
                        .push_str(&segment.source);
                }
            } else if let Some(anchor) = segment.anchor.as_deref() {
                current = Some(anchor);
                stored_sources.insert(anchor, segment.source.clone());
            } else {
                current = None;
            }
        }
        let changed: Vec<&&str> = stored_sources
            .iter()
            .filter(|(anchor, source)| {
                fresh_by_anchor
                    .get(*anchor)
                    .is_some_and(|fresh| &fresh.source != *source)
            })
            .map(|(anchor, _)| anchor)
            .collect();
        if !changed.is_empty() {
            let preview = changed
                .iter()
                .take(3)
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(Error::Store(format!(
                "EPUB source does not match translation state: {href} content changed ({preview})"
            )));
        }
        let fresh_meta: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
            fresh_by_anchor
                .iter()
                .map(|(anchor, segment)| ((*anchor).to_string(), segment.meta.clone()))
                .collect();
        prepared.insert(href.clone(), (segments.clone(), template, fresh_meta));
    }
    let mut source_ids_by_resource: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut source_link_targets: BTreeMap<(String, String), String> = BTreeMap::new();
    if options.bilingual {
        for (href, (segments, template, _)) in &prepared {
            let dom = parse_html(template);
            let (occupied, tn_id_index) = index_dom_ids(&dom);
            let (by_anchor, src_by_anchor, kind_by_anchor, _stored) = segment_render_maps(segments);
            let source_ids = build_source_anchor_ids(
                &by_anchor,
                &src_by_anchor,
                &kind_by_anchor,
                &tn_id_index,
                &occupied,
            );
            for (fragment, anchor) in fragment_anchor_map(template) {
                if fragment.is_empty() {
                    continue;
                }
                if let Some(anchor) = anchor {
                    if let Some(source_id) = source_ids.get(&anchor) {
                        source_link_targets.insert((href.clone(), fragment), source_id.clone());
                    }
                }
            }
            source_ids_by_resource.insert(href.clone(), source_ids);
        }
    }
    let mut rendered = BTreeMap::new();
    for (_, href) in &resources {
        let Some((segments, template, fresh_meta)) = prepared.get(href) else {
            continue;
        };
        rendered.insert(
            href.clone(),
            render_segments_html(
                template,
                segments,
                Some(fresh_meta),
                options.bilingual,
                &options.order,
                options.preserve_source_style,
                source_lang,
                options.preserve_source_ruby,
                href,
                source_ids_by_resource.get(href).cloned(),
                Some(source_link_targets.clone()),
            ),
        );
    }
    Ok(rendered)
}

/// Update OPF metadata with export title + target language (oracle `_rewrite_opf_metadata`).
pub fn rewrite_opf_metadata(
    data: &[u8],
    book_title: &str,
    lang: &str,
    force_horizontal: bool,
) -> Vec<u8> {
    // OPF is XML; parse with the lenient HTML DOM (tag names lowercased).
    // `dc:title`/`dc:language` survive as literal tag names.
    let text = decode_markup(data);
    let mut dom = parse_html(&text);
    if !book_title.is_empty() {
        let title_el = dom.find_descendant(0, |d| matches!(dom.tag(d), Some("dc:title" | "title")));
        if let Some(el) = title_el {
            dom.clear(el);
            dom.append(
                el,
                Node::Text {
                    text: book_title.to_string(),
                },
            );
        }
    }
    let lang_el = dom.find_descendant(0, |d| {
        matches!(dom.tag(d), Some("dc:language" | "language"))
    });
    let lang_el = lang_el.or_else(|| {
        dom.find_descendant(0, |d| dom.tag(d) == Some("metadata"))
            .map(|metadata| {
                dom.append(
                    metadata,
                    Node::Element {
                        tag: "dc:language".to_string(),
                        attrs: Vec::new(),
                        children: Vec::new(),
                    },
                )
            })
    });
    if let Some(el) = lang_el {
        dom.clear(el);
        dom.append(
            el,
            Node::Text {
                text: lang.to_string(),
            },
        );
    }
    if force_horizontal {
        for spine in dom.find_all_descendants(0, |d| dom.tag(d) == Some("spine")) {
            dom.set_attr(spine, "page-progression-direction", "ltr");
        }
    }
    dom.serialize().into_bytes()
}

/// Enumerate NCX navPoints in direct-child preorder (oracle `_ncx_nav_points`).
fn ncx_nav_points(dom: &Dom) -> Vec<NodeId> {
    let Some(nav_map) = dom.find_descendant(0, |d| {
        dom.tag(d).is_some_and(|t| t.eq_ignore_ascii_case("navmap"))
    }) else {
        return Vec::new();
    };
    let mut points = Vec::new();
    let mut stack: Vec<NodeId> = dom
        .children(nav_map)
        .into_iter()
        .filter(|c| {
            dom.tag(*c)
                .is_some_and(|t| t.eq_ignore_ascii_case("navpoint"))
        })
        .rev()
        .collect();
    while let Some(point) = stack.pop() {
        points.push(point);
        for child in dom
            .children(point)
            .into_iter()
            .filter(|c| {
                dom.tag(*c)
                    .is_some_and(|t| t.eq_ignore_ascii_case("navpoint"))
            })
            .rev()
        {
            stack.push(child);
        }
    }
    points
}

/// NAV TOC scopes (oracle `nav_toc_scopes`): typed navs, first nav, or whole doc.
fn nav_toc_scopes(dom: &Dom) -> Vec<NodeId> {
    let navs: Vec<NodeId> = dom.find_all_descendants(0, |d| dom.tag(d) == Some("nav"));
    let typed: Vec<NodeId> = navs
        .iter()
        .copied()
        .filter(|nav| {
            let combined = format!(
                "{} {}",
                dom.attr(*nav, "epub:type").unwrap_or(""),
                dom.attr(*nav, "type").unwrap_or("")
            );
            combined.split_whitespace().any(|w| w == "toc")
        })
        .collect();
    if !typed.is_empty() {
        return typed;
    }
    if let Some(first) = navs.first() {
        return vec![*first];
    }
    vec![0]
}

/// Root list within a NAV scope (oracle `nav_root_list`).
fn nav_root_list(dom: &Dom, scope: NodeId) -> Option<NodeId> {
    if let Some(direct) = dom
        .children(scope)
        .into_iter()
        .find(|c| dom.tag(*c) == Some("ol"))
    {
        return Some(direct);
    }
    dom.find_descendant(scope, |d| dom.tag(d) == Some("ol"))
}

/// Enumerate NAV labels in reader preorder (oracle `_nav_label_nodes`).
fn nav_label_nodes(dom: &Dom) -> Vec<(NodeId, String)> {
    let mut labels = Vec::new();
    fn walk_list(dom: &Dom, list: NodeId, labels: &mut Vec<(NodeId, String)>) {
        for child in dom.children(list) {
            if dom.tag(child) != Some("li") {
                continue;
            }
            let label = dom
                .direct_child(child, "a")
                .or_else(|| dom.direct_child(child, "span"));
            if let Some(label) = label {
                let href = dom.attr(label, "href").unwrap_or("").to_string();
                labels.push((label, href));
            }
            if let Some(nested) = dom.direct_child(child, "ol") {
                walk_list(dom, nested, labels);
            }
        }
    }
    for scope in nav_toc_scopes(dom) {
        if let Some(root) = nav_root_list(dom, scope) {
            walk_list(dom, root, &mut labels);
        }
    }
    labels
}

/// Translated TOC title with original fallback (oracle `_translated_toc_title`).
fn translated_toc_title(entry: &serde_json::Map<String, serde_json::Value>) -> String {
    entry
        .get("title_translated")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| entry.get("title").and_then(|v| v.as_str()))
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Replace visible NCX/NAV titles (oracle `_rewrite_toc`).
pub fn rewrite_toc(
    data: &[u8],
    entries: &[serde_json::Map<String, serde_json::Value>],
    is_ncx: bool,
    toc_path: &str,
) -> Vec<u8> {
    let indexed: BTreeMap<usize, &serde_json::Map<String, serde_json::Value>> = entries
        .iter()
        .filter(|e| e.get("toc_path").and_then(|v| v.as_str()) == Some(toc_path))
        .filter_map(|e| {
            e.get("node_index")
                .and_then(json_int)
                .and_then(|i| usize::try_from(i).ok().map(|index| (index, e)))
        })
        .collect();
    if is_ncx {
        let mut dom = parse_html(&decode_markup(data));
        for (node_index, point) in ncx_nav_points(&dom).into_iter().enumerate() {
            // Oracle reads `navLabel/text` via direct child + descendant find.
            let Some(nav_label) = dom.children(point).into_iter().find(|c| {
                dom.tag(*c)
                    .is_some_and(|t| t.eq_ignore_ascii_case("navlabel"))
            }) else {
                continue;
            };
            let Some(label) = dom.find_descendant(nav_label, |d| {
                dom.tag(d).is_some_and(|t| t.eq_ignore_ascii_case("text"))
            }) else {
                continue;
            };
            let Some(entry) = indexed.get(&node_index) else {
                continue;
            };
            let raw_src = dom
                .children(point)
                .into_iter()
                .find(|c| {
                    dom.tag(*c)
                        .is_some_and(|t| t.eq_ignore_ascii_case("content"))
                })
                .and_then(|content| dom.attr(content, "src"))
                .unwrap_or("")
                .to_string();
            if let Some(expected) = entry.get("raw_href").and_then(|v| v.as_str()) {
                if expected != raw_src {
                    continue;
                }
            }
            let title = translated_toc_title(entry);
            if !title.is_empty() {
                dom.clear(label);
                dom.append(label, Node::Text { text: title });
            }
        }
        return dom.serialize().into_bytes();
    }
    let text = decode_markup(data);
    let mut dom = parse_html(&text);
    let toc_navs: Vec<NodeId> = dom
        .find_all_descendants(0, |d| dom.tag(d) == Some("nav"))
        .into_iter()
        .filter(|nav| {
            let combined = format!(
                "{} {} {}",
                dom.attr(*nav, "epub:type").unwrap_or(""),
                dom.attr(*nav, "type").unwrap_or(""),
                dom.attr(*nav, "role").unwrap_or("")
            );
            combined.split_whitespace().any(|w| w == "toc")
                || dom.attr(*nav, "role") == Some("doc-toc")
        })
        .collect();
    if toc_navs.is_empty()
        && !(toc_path.to_lowercase().contains("nav") || toc_path.to_lowercase().contains("toc"))
    {
        return data.to_vec();
    }
    for (node_index, (label, raw_href)) in nav_label_nodes(&dom).into_iter().enumerate() {
        let Some(entry) = indexed.get(&node_index) else {
            continue;
        };
        if let Some(expected) = entry.get("raw_href").and_then(|v| v.as_str()) {
            if expected != raw_href {
                continue;
            }
        }
        let title = translated_toc_title(entry);
        if !title.is_empty() {
            dom.clear(label);
            dom.append(label, Node::Text { text: title });
        }
    }
    dom.serialize().into_bytes()
}

/// Detect EPUB3 TOC navigation (oracle `_is_nav`).
pub fn is_nav(data: &[u8]) -> bool {
    let has_nav = data.windows(4).any(|w| w == b"<nav");
    has_nav
        && (data.windows(16).any(|w| w == b"epub:type=\"toc\"")
            || data.windows(16).any(|w| w == b"epub:type='toc'")
            || data.windows(11).any(|w| w == b"type=\"toc\"")
            || data.windows(15).any(|w| w == b"role=\"doc-toc\""))
}

/// Whether any OPF/CSS/XHTML member declares vertical layout (oracle `_epub_looks_vertical`).
pub fn epub_looks_vertical<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> bool {
    let names: Vec<String> = archive
        .file_names()
        .collect::<std::result::Result<Vec<_>, _>>()
        .map(|names| names.into_iter().map(|s| s.into_owned()).collect())
        .unwrap_or_default();
    for name in names {
        let low = name.to_lowercase();
        if !(low.ends_with(".opf")
            || low.ends_with(".css")
            || low.ends_with(".xhtml")
            || low.ends_with(".html")
            || low.ends_with(".htm"))
        {
            continue;
        }
        let mut data = Vec::new();
        {
            use std::io::Read;
            let Ok(mut entry) = archive.by_name(&name) else {
                continue;
            };
            if entry.read_to_end(&mut data).is_err() {
                continue;
            }
        }
        if looks_vertical_bytes(&data) {
            return true;
        }
    }
    false
}

/// Byte-level vertical-layout markers (case-insensitive, no regex dependency).
fn looks_vertical_bytes(data: &[u8]) -> bool {
    let lower: Vec<u8> = data.iter().map(|b| b.to_ascii_lowercase()).collect();
    contains_writing_mode_vertical(&lower)
        || contains_page_progression_rtl(&lower)
        || contains_vrtl_class(&lower)
}

/// `writing-mode\s*:\s*(vertical-rl|vertical-lr|tb-rl)` (case already lowered).
fn contains_writing_mode_vertical(lower: &[u8]) -> bool {
    let mut i = 0;
    while i + 12 <= lower.len() {
        if &lower[i..i + 12] == b"writing-mode" {
            let mut j = i + 12;
            while j < lower.len() && lower[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < lower.len() && lower[j] == b':' {
                j += 1;
                while j < lower.len() && lower[j].is_ascii_whitespace() {
                    j += 1;
                }
                for value in [
                    b"vertical-rl".as_slice(),
                    b"vertical-lr".as_slice(),
                    b"tb-rl".as_slice(),
                ] {
                    if lower[j..].starts_with(value) {
                        return true;
                    }
                }
            }
        }
        i += 1;
    }
    false
}

/// `page-progression-direction\s*=\s*['"]rtl['"]` (case already lowered).
fn contains_page_progression_rtl(lower: &[u8]) -> bool {
    let needle = b"page-progression-direction";
    let mut i = 0;
    while i + needle.len() <= lower.len() {
        if &lower[i..i + needle.len()] == needle {
            let mut j = i + needle.len();
            while j < lower.len() && lower[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < lower.len() && lower[j] == b'=' {
                j += 1;
                while j < lower.len() && lower[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j + 5 <= lower.len()
                    && (lower[j] == b'\'' || lower[j] == b'"')
                    && &lower[j + 1..j + 4] == b"rtl"
                    && (lower[j + 4] == b'\'' || lower[j + 4] == b'"')
                {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

/// `\bclass\s*=\s*['"][^'"]*\bvrtl\b` (case already lowered).
fn contains_vrtl_class(lower: &[u8]) -> bool {
    let mut i = 0;
    while i + 5 <= lower.len() {
        if &lower[i..i + 5] == b"class"
            && (i == 0 || !(lower[i - 1].is_ascii_alphanumeric() || lower[i - 1] == b'_'))
        {
            let mut j = i + 5;
            while j < lower.len() && lower[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < lower.len() && lower[j] == b'=' {
                j += 1;
                while j < lower.len() && lower[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < lower.len() && (lower[j] == b'\'' || lower[j] == b'"') {
                    let quote = lower[j];
                    j += 1;
                    let start = j;
                    while j < lower.len() && lower[j] != quote {
                        j += 1;
                    }
                    let value = &lower[start..j];
                    if value
                        .split(|b: &u8| !b.is_ascii_alphanumeric() && *b != b'_')
                        .any(|w| w == b"vrtl")
                    {
                        return true;
                    }
                }
            }
        }
        i += 1;
    }
    false
}

/// Normalize an XML declaration's encoding to UTF-8 (oracle `_XML_ENCODING`).
fn normalize_xml_encoding(text: &str) -> String {
    let Some(decl_start) = text.find("<?xml") else {
        return text.to_string();
    };
    let Some(decl_end) = text[decl_start..].find("?>") else {
        return text.to_string();
    };
    let decl_end = decl_start + decl_end;
    let decl = &text[decl_start..decl_end];
    let lower = decl.to_lowercase();
    let Some(enc_pos) = lower.find("encoding") else {
        return text.to_string();
    };
    // Word boundary before `encoding`.
    if enc_pos > 0 {
        let before = decl.as_bytes()[enc_pos - 1];
        if before.is_ascii_alphanumeric() || before == b'_' {
            return text.to_string();
        }
    }
    let mut rest = &decl[enc_pos + 8..];
    rest = rest.trim_start();
    if !rest.starts_with('=') {
        return text.to_string();
    }
    rest = rest[1..].trim_start();
    let quote = rest.chars().next().unwrap_or('\0');
    if quote != '"' && quote != '\'' {
        return text.to_string();
    }
    let after_quote = &rest[1..];
    let Some(close) = after_quote.find(quote) else {
        return text.to_string();
    };
    let value_start = text.len() - (text[decl_start..].len() - (decl.len() - rest.len() + 1));
    let _ = value_start;
    // Rebuild: prefix + `"utf-8"` + suffix.
    let prefix_end = decl_start + (decl.len() - rest.len()) + 1;
    let suffix_start = prefix_end + close;
    format!(
        "{}\"{}\"{}",
        &text[..prefix_end],
        "utf-8",
        &text[suffix_start..]
    )
}

/// Set document language + inject layout/bilingual styles (oracle `_rewrite_html_document`).
pub fn rewrite_html_document(
    data: &[u8],
    lang: &str,
    force_horizontal: bool,
    bilingual: bool,
) -> Vec<u8> {
    let text = decode_markup(data);
    let mut dom = parse_html(&text);
    let Some(html) = dom.find_descendant(0, |d| dom.tag(d) == Some("html")) else {
        return text.into_bytes();
    };
    dom.set_attr(html, "lang", lang);
    dom.set_attr(html, "xml:lang", lang);
    if let Some(classes) = dom.attr(html, "class").map(str::to_string) {
        if classes.split_whitespace().any(|c| c == "vrtl") {
            let kept: Vec<&str> = classes
                .split_whitespace()
                .filter(|c| *c != "vrtl")
                .collect();
            dom.set_attr(html, "class", &kept.join(" "));
        }
    }
    if force_horizontal
        && dom
            .find_descendant(0, |d| dom.attr(d, "id") == Some(HORIZONTAL_OVERRIDE_ID))
            .is_none()
    {
        let head = dom.direct_child(html, "head").unwrap_or_else(|| {
            let head = dom.append(
                0,
                Node::Element {
                    tag: "head".to_string(),
                    attrs: Vec::new(),
                    children: Vec::new(),
                },
            );
            match dom.children(html).into_iter().next() {
                Some(first) => dom.move_before(head, first),
                None => dom.move_to(head, html),
            }
            head
        });
        let style = dom.append(
            head,
            Node::Element {
                tag: "style".to_string(),
                attrs: Vec::new(),
                children: Vec::new(),
            },
        );
        dom.set_attr(style, "id", HORIZONTAL_OVERRIDE_ID);
        dom.append(
            style,
            Node::Text {
                text: HORIZONTAL_CSS.to_string(),
            },
        );
    }
    if bilingual
        && dom
            .find_descendant(0, |d| dom.attr(d, "id") == Some(BILINGUAL_STYLE_ID))
            .is_none()
    {
        let head = dom.direct_child(html, "head").unwrap_or_else(|| {
            let head = dom.append(
                0,
                Node::Element {
                    tag: "head".to_string(),
                    attrs: Vec::new(),
                    children: Vec::new(),
                },
            );
            match dom.children(html).into_iter().next() {
                Some(first) => dom.move_before(head, first),
                None => dom.move_to(head, html),
            }
            head
        });
        let style = dom.append(
            head,
            Node::Element {
                tag: "style".to_string(),
                attrs: Vec::new(),
                children: Vec::new(),
            },
        );
        dom.set_attr(style, "id", BILINGUAL_STYLE_ID);
        dom.append(
            style,
            Node::Text {
                text: BILINGUAL_CSS.to_string(),
            },
        );
    }
    normalize_xml_encoding(&dom.serialize()).into_bytes()
}

/// Copy an EPUB, replacing body resources, TOC titles, metadata (oracle `_assemble_epub`).
///
/// `document` is the translated state (chapters carry targets); `source_path`
/// is the original EPUB. Writes atomically via a sibling temp file.
pub fn assemble_epub(
    document: &Document,
    source_path: &Path,
    out_path: &Path,
    options: &AssembleOptions,
) -> Result<()> {
    if document.target_lang.is_empty() {
        return Err(Error::Store("State is missing target_lang".to_string()));
    }
    let target_lang = epub_lang(&document.target_lang);
    let meta = &document.meta;
    let toc_entries: Vec<serde_json::Map<String, serde_json::Value>> = meta
        .get("toc_entries")
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|v| v.as_object().cloned())
                .collect()
        })
        .unwrap_or_default();
    let mut toc_paths: BTreeSet<String> = meta
        .get("toc_paths")
        .and_then(|v| v.as_array())
        .map(|paths| {
            paths
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for entry in &toc_entries {
        if let Some(path) = entry.get("toc_path").and_then(|v| v.as_str()) {
            if !path.is_empty() {
                toc_paths.insert(path.to_string());
            }
        }
    }
    let ncx_paths: BTreeSet<&str> = toc_entries
        .iter()
        .filter(|e| e.get("kind").and_then(|v| v.as_str()) == Some("ncx"))
        .filter_map(|e| e.get("toc_path").and_then(|v| v.as_str()))
        .collect();
    let book_title = export_book_title(&document.title, &document.target_lang, options.bilingual);
    let file = File::open(source_path).map_err(|e| Error::Io {
        path: source_path.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| Error::Store(format!("Invalid EPUB zip: {e}")))?;
    let force_horizontal = epub_looks_vertical(&mut archive);
    let rendered = render_epub_resources(
        &mut archive,
        &document.chapters,
        meta,
        options,
        &document.title,
        &document.source_lang,
    )?;
    let names: Vec<String> = archive
        .file_names()
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| Error::Store(format!("Invalid EPUB zip: {e}")))?
        .into_iter()
        .map(|s| s.into_owned())
        .collect();
    // Read all members before opening the writer (single-reader borrow).
    let mut members: Vec<(String, Vec<u8>)> = Vec::with_capacity(names.len());
    for name in &names {
        let mut entry = archive
            .by_name(name)
            .map_err(|e| Error::Store(format!("Invalid EPUB zip member {name}: {e}")))?;
        let mut data = Vec::new();
        use std::io::Read;
        entry.read_to_end(&mut data).map_err(|e| Error::Io {
            path: source_path.to_path_buf(),
            message: e.to_string(),
        })?;
        members.push((name.clone(), data));
    }
    drop(archive);
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                path: parent.to_path_buf(),
                message: e.to_string(),
            })?;
        }
    }
    let tmp_path = out_path.with_extension("epub.tmp");
    {
        let out_file = File::create(&tmp_path).map_err(|e| Error::Io {
            path: tmp_path.clone(),
            message: e.to_string(),
        })?;
        let mut writer = zip::ZipWriter::new(out_file);
        for (name, data) in &members {
            let low = name.to_lowercase();
            let output: Vec<u8> = if name == "mimetype" {
                data.clone()
            } else if low.ends_with(".opf") {
                rewrite_opf_metadata(data, &book_title, target_lang, force_horizontal)
            } else if low.ends_with(".ncx") || ncx_paths.contains(name.as_str()) {
                rewrite_toc(data, &toc_entries, true, name)
            } else if low.ends_with(".xhtml") || low.ends_with(".html") || low.ends_with(".htm") {
                let mut html_data: Vec<u8> = rendered
                    .get(name)
                    .map(|rendered| rendered.as_bytes().to_vec())
                    .unwrap_or_else(|| data.clone());
                if toc_paths.contains(name) || is_nav(&html_data) {
                    html_data = rewrite_toc(&html_data, &toc_entries, false, name);
                }
                rewrite_html_document(
                    &html_data,
                    target_lang,
                    force_horizontal,
                    options.bilingual && !options.preserve_source_style,
                )
            } else {
                data.clone()
            };
            let zip_options = zip::write::SimpleFileOptions::default().compression_method(
                if name == "mimetype" {
                    zip::CompressionMethod::Stored
                } else {
                    zip::CompressionMethod::Deflated
                },
            );
            writer
                .start_file(name, zip_options)
                .map_err(|e| Error::Io {
                    path: tmp_path.clone(),
                    message: e.to_string(),
                })?;
            use std::io::Write;
            writer.write_all(&output).map_err(|e| Error::Io {
                path: tmp_path.clone(),
                message: e.to_string(),
            })?;
        }
        writer.finish().map_err(|e| Error::Io {
            path: tmp_path.clone(),
            message: e.to_string(),
        })?;
    }
    std::fs::rename(&tmp_path, out_path).map_err(|e| Error::Io {
        path: out_path.to_path_buf(),
        message: e.to_string(),
    })?;
    Ok(())
}

/// Parsed OPF package lookup (re-exported use for golden tests).
#[doc(hidden)]
pub fn __parse_package_for_tests(source_path: &Path) -> Result<OpfPackage> {
    let file = File::open(source_path).map_err(|e| Error::Io {
        path: source_path.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| Error::Store(format!("Invalid EPUB zip: {e}")))?;
    let opf_path = find_opf_path(&mut archive)?;
    parse_opf(&mut archive, &opf_path)
}

/// Heading level: segment meta wins, chapter meta is the fallback (oracle
/// `_heading_level`).
pub fn heading_level(chapter: &Chapter, segment: Option<&Segment>) -> i64 {
    let mut level = chapter
        .meta
        .get("heading_level")
        .and_then(|v| v.as_i64())
        .unwrap_or(1);
    if let Some(segment) = segment {
        if let Some(value) = segment.meta.get("heading_level").and_then(|v| v.as_i64()) {
            level = value;
        }
    }
    if level > 0 {
        level
    } else {
        1
    }
}

/// Merge continuations into `(kind, target, source, level)` paragraphs
/// (oracle `_merged_paragraphs`).
pub fn merged_paragraphs(chapter: &Chapter) -> Vec<(String, String, String, i64)> {
    let mut kinds = Vec::new();
    let mut targets: Vec<String> = Vec::new();
    let mut sources: Vec<String> = Vec::new();
    let mut levels = Vec::new();
    for segment in &chapter.segments {
        if segment.source.trim().is_empty() {
            continue;
        }
        if segment.cont && !targets.is_empty() {
            let last = targets.len() - 1;
            targets[last].push_str(seg_text(segment));
            sources[last].push_str(&segment.source);
        } else {
            kinds.push(segment.kind.clone());
            targets.push(seg_text(segment).to_string());
            sources.push(segment.source.clone());
            levels.push(heading_level(chapter, Some(segment)));
        }
    }
    kinds
        .into_iter()
        .zip(targets)
        .zip(sources)
        .zip(levels)
        .map(|(((kind, target), source), level)| (kind, target, source, level))
        .collect()
}

/// Order a bilingual pair (oracle `_ordered_pair`).
pub fn ordered_pair<'a>(source: &'a str, target: &'a str, order: &str) -> (&'a str, &'a str) {
    if order == "source_first" {
        (source, target)
    } else {
        (target, source)
    }
}

/// Write a file atomically via same-directory temp + rename.
pub fn write_atomic(out_path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                path: parent.to_path_buf(),
                message: e.to_string(),
            })?;
        }
    }
    let tmp_path = out_path.with_extension(
        out_path
            .extension()
            .map(|ext| format!("{}.tmp", ext.to_string_lossy()))
            .unwrap_or_else(|| "tmp".to_string()),
    );
    std::fs::write(&tmp_path, bytes).map_err(|e| Error::Io {
        path: tmp_path.clone(),
        message: e.to_string(),
    })?;
    std::fs::rename(&tmp_path, out_path).map_err(|e| Error::Io {
        path: out_path.to_path_buf(),
        message: e.to_string(),
    })?;
    Ok(())
}

/// Assemble plain text or Markdown (oracle `text_writer`).
pub fn assemble_text(
    document: &Document,
    out_path: &Path,
    options: &AssembleOptions,
    markdown: bool,
) -> Result<()> {
    let mut chapter_blocks = Vec::new();
    for chapter in &document.chapters {
        let mut blocks = Vec::new();
        for (kind, mut target, source, level) in merged_paragraphs(chapter) {
            if kind == KIND_HEADING && markdown {
                target = format!("{} {target}", "#".repeat(level.clamp(1, 6) as usize));
            }
            let src = if options.bilingual && kind != KIND_HEADING {
                bilingual_source(&source, &target)
            } else {
                String::new()
            };
            if src.is_empty() {
                blocks.push(target);
            } else {
                let (first, second) = ordered_pair(&src, &target, &options.order);
                blocks.push(first.to_string());
                blocks.push(second.to_string());
            }
        }
        chapter_blocks.push(blocks.join("\n\n"));
    }
    write_atomic(
        out_path,
        format!("{}\n", chapter_blocks.join("\n\n")).as_bytes(),
    )
}

/// Escape HTML text (oracle `html.escape`).
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Assemble a standalone HTML document (oracle `html_writer`, template-free
/// path). EPUB-template backfill stays in `assemble_epub`.
pub fn assemble_html(
    document: &Document,
    out_path: &Path,
    options: &AssembleOptions,
) -> Result<()> {
    if document.target_lang.is_empty() {
        return Err(Error::Store("State is missing target_lang".to_string()));
    }
    let mut head_html = document
        .meta
        .get("head_html")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if !head_html
        .replace(' ', "")
        .to_lowercase()
        .contains("charset")
    {
        head_html = format!("<meta charset=\"utf-8\"/>\n{head_html}");
    }
    if options.bilingual
        && !options.preserve_source_style
        && !head_html.contains(BILINGUAL_STYLE_ID)
    {
        head_html += &format!("<style id=\"{BILINGUAL_STYLE_ID}\">\n{BILINGUAL_CSS}</style>");
    }
    let mut body_parts = Vec::new();
    for chapter in &document.chapters {
        for (kind, target, source, level) in merged_paragraphs(chapter) {
            let target_html = if kind == KIND_HEADING {
                format!(
                    "<h{level}>{}</h{level}>",
                    escape_html(&target),
                    level = level.clamp(1, 6)
                )
            } else {
                format!("<p>{}</p>", escape_html(&target))
            };
            let src = if options.bilingual && kind != KIND_HEADING {
                bilingual_source(&source, &target)
            } else {
                String::new()
            };
            if src.is_empty() {
                body_parts.push(target_html);
                continue;
            }
            let source_html = format!("<p class=\"tn-source\">{}</p>", escape_html(&src));
            if options.order == "source_first" {
                body_parts.push(source_html);
                body_parts.push(target_html);
            } else {
                body_parts.push(target_html);
                body_parts.push(source_html);
            }
        }
    }
    let full_html = format!(
        "<!DOCTYPE html>\n<html lang=\"{}\">\n<head>\n{head_html}\n</head>\n<body>\n{}\n</body>\n</html>",
        escape_html(epub_lang(&document.target_lang)),
        body_parts.join("")
    );
    write_atomic(out_path, full_html.as_bytes())
}
