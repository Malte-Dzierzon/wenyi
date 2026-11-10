//! XHTML resource annotation: stable segments + inline/annotation metadata.
//!
//! Oracle: `markup/segments.py` + `markup/annotations.py` + `markup/ruby.py`.
//! `annotate_resource` mirrors `annotate_epub_resource`: same targets, anchors,
//! text extraction, and meta shapes.

use std::collections::{BTreeMap, BTreeSet};

use crate::document::anchors::resolve_epub_href;
use crate::document::markup::{parse_html, Dom, NodeId};
use crate::document::models::{Segment, KIND_HEADING, KIND_TEXT};

/// Block-level translatable tags (oracle `_BLOCK_TAGS`).
const BLOCK_TAGS: [&str; 15] = [
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "blockquote",
    "td",
    "th",
    "dt",
    "dd",
    "figcaption",
    "div",
];

/// True block tags (div excluded; oracle `_BLOCK_TAGS` without div).
const STRICT_BLOCK_TAGS: [&str; 14] = [
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "blockquote",
    "td",
    "th",
    "dt",
    "dd",
    "figcaption",
];

/// Heading tags.
const HEADING_TAGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];

/// Atomic inline tags preserved verbatim (oracle `_ATOMIC_INLINE_TAGS`).
const ATOMIC_INLINE_TAGS: [&str; 12] = [
    "audio", "canvas", "embed", "hr", "iframe", "img", "math", "object", "source", "svg", "video",
    "br",
];

/// Inline metadata key.
pub const INLINE_META_KEY: &str = "epub_inline";
/// Inline id attribute.
pub const INLINE_ID_ATTR: &str = "data-tn-inline-id";
/// Annotation metadata key.
pub const ANNOTATION_META_KEY: &str = "epub_annotations";
/// Annotation id attribute.
pub const ANNOTATION_ID_ATTR: &str = "data-tn-annotation-id";
/// Line-wrapper attribute for br-split runs.
pub const LINE_WRAPPER_ATTR: &str = "data-tn-line";
/// Segment id attribute.
pub const SEGMENT_ID_ATTR: &str = "data-tn-id";
/// Ruby reading markers (oracle `ruby.py`).
pub const RUBY_MARK_LEFT: char = '〘';
pub const RUBY_MARK_RIGHT: char = '〙';

/// Marker-only text alphabet (oracle `_ANNOTATION_MARKER_ONLY`).
fn is_marker_only(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|c| {
            c.is_ascii_digit()
                || c.is_whitespace()
                || matches!(
                    c,
                    '*' | '＊'
                        | '※'
                        | '†'
                        | '‡'
                        | '['
                        | ']'
                        | '('
                        | ')'
                        | '〔'
                        | '〕'
                        | '（'
                        | '）'
                        | '{'
                        | '}'
                        | '↩'
                        | '↵'
                        | '←'
                        | '↑'
                        | '↓'
                        | '⤶'
                        | '.'
                        | '·'
                        | ':'
                        | '：'
                        | '-'
                )
        })
}

/// Annotation hint on attrs/fragment (oracle `_ANNOTATION_HINT`).
fn hint_matches(text: &str) -> bool {
    // (?:^|[^a-z0-9])(?:note|noteref|footnote|endnote|fn|jpref|jpnote)(?:[-_]?\d+)?(?:$|[^a-z0-9])
    let lower = text.to_lowercase();
    let words = [
        "note", "noteref", "footnote", "endnote", "fn", "jpref", "jpnote",
    ];
    contains_word(&lower, &words)
}

fn contains_word(haystack: &str, words: &[&str]) -> bool {
    let bytes = haystack.as_bytes();
    for word in words {
        let mut start = 0;
        while let Some(pos) = haystack[start..].find(word) {
            let abs = start + pos;
            let before_ok = abs == 0 || !bytes[abs - 1].is_ascii_alphanumeric();
            let mut end = abs + word.len();
            // Optional [-_]?\d+.
            if end < bytes.len() && (bytes[end] == b'-' || bytes[end] == b'_') {
                end += 1;
            }
            let digits = end;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let after_ok = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
            if before_ok && after_ok && (end > digits || digits == abs + word.len()) {
                return true;
            }
            start = abs + 1;
            if start >= haystack.len() {
                break;
            }
        }
    }
    false
}

/// Note-target hint (oracle `_NOTE_TARGET_HINT`).
fn has_note_target_hint(text: &str) -> bool {
    contains_word(&text.to_lowercase(), &["note", "footnote", "endnote", "fn"])
}

/// Short `n1`-style identity (oracle `_SHORT_NOTE_IDENTITY`).
fn is_short_note_identity(value: &str) -> bool {
    let lower = value.to_lowercase();
    let rest = lower.strip_prefix('n').unwrap_or("\u{10ffff}");
    let rest = rest.strip_prefix(['-', '_']).unwrap_or(rest);
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) && value.len() - rest.len() <= 2
}

/// Semantic tokens from epub:type/role/rel (oracle `_semantic_tokens`).
fn semantic_tokens(dom: &Dom, id: NodeId) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    for key in ["epub:type", "role", "rel"] {
        if let Some(value) = dom.attr(id, key) {
            for raw in value.split_whitespace() {
                let normalized = raw.trim().to_lowercase();
                if normalized.is_empty() {
                    continue;
                }
                tokens.insert(normalized.clone());
                if let Some(suffix) = normalized.rsplit(':').next() {
                    tokens.insert(suffix.to_string());
                }
            }
        }
    }
    tokens
}

fn inside_note_body(dom: &Dom, id: NodeId) -> bool {
    let mut chain = vec![id];
    chain.extend(dom.ancestors(id));
    chain.into_iter().any(|n| {
        semantic_tokens(dom, n)
            .intersection(
                &[
                    "footnote",
                    "endnote",
                    "rearnote",
                    "doc-footnote",
                    "doc-endnote",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            )
            .next()
            .is_some()
    })
}

fn node_identity(dom: &Dom, id: NodeId) -> String {
    let mut parts = Vec::new();
    for key in ["id", "name", "class"] {
        if let Some(value) = dom.attr(id, key) {
            parts.push(value.to_string());
        }
    }
    parts.join(" ")
}

fn has_note_identity(dom: &Dom, id: NodeId) -> bool {
    has_note_target_hint(&node_identity(dom, id))
}

fn has_short_note_identity(dom: &Dom, id: NodeId) -> bool {
    ["id", "name"]
        .into_iter()
        .any(|key| dom.attr(id, key).is_some_and(is_short_note_identity))
}

fn implicit_note_body_scope(dom: &Dom, id: NodeId) -> Option<NodeId> {
    let mut chain = vec![id];
    chain.extend(dom.ancestors(id));
    chain.into_iter().find(|n| {
        matches!(
            dom.tag(*n),
            Some("aside" | "li" | "dd" | "p" | "div" | "section")
        ) && has_note_identity(dom, *n)
    })
}

fn short_note_body_scope(dom: &Dom, id: NodeId) -> Option<NodeId> {
    let mut chain = vec![id];
    chain.extend(dom.ancestors(id));
    chain.into_iter().find(|n| {
        matches!(dom.tag(*n), Some("aside" | "li" | "dd" | "p")) && has_short_note_identity(dom, *n)
    })
}

fn inside_implicit_note_body(dom: &Dom, id: NodeId) -> bool {
    if implicit_note_body_scope(dom, id).is_some() {
        return true;
    }
    // Short `n1`-style scope: only when the link text is marker-only and sits
    // asymmetrically within the scope (oracle `_inside_implicit_note_body`).
    let Some(scope) = short_note_body_scope(dom, id) else {
        return false;
    };
    if !is_marker_only(&dom.get_text(id, "", true)) {
        return false;
    }
    let (before, after) = surrounding_text(dom, scope, id);
    !before.trim().is_empty() != !after.trim().is_empty()
}

/// Internal link check (oracle `_is_internal_link`).
fn is_internal_link(dom: &Dom, id: NodeId) -> bool {
    let Some(href) = dom.attr(id, "href") else {
        return false;
    };
    let href = href.trim();
    if href.is_empty() {
        return false;
    }
    // No scheme/netloc: reject `scheme:...` and `//host`.
    if href.starts_with("//") {
        return false;
    }
    if let Some(colon) = href.find(':') {
        // A colon before any '/', '?', '#' indicates a scheme (mailto:, http:, ...).
        let before = &href[..colon];
        if !before.contains(['/', '?', '#']) && !before.is_empty() {
            return false;
        }
    }
    true
}

/// Nearest sup/sub/span marker wrapper (oracle `_nearest_marker_wrapper`).
fn nearest_marker_wrapper(dom: &Dom, link: NodeId, block: NodeId) -> Option<NodeId> {
    fn is_wrapper(dom: &Dom, id: NodeId) -> bool {
        match dom.tag(id) {
            Some("sup" | "sub") => true,
            Some("span") => {
                let classes: BTreeSet<String> = dom
                    .attr(id, "class")
                    .unwrap_or("")
                    .split_whitespace()
                    .map(|s| s.to_lowercase())
                    .collect();
                if classes
                    .intersection(
                        &["sup", "super", "superscript", "sub", "subscript"]
                            .into_iter()
                            .map(str::to_string)
                            .collect(),
                    )
                    .next()
                    .is_some()
                {
                    return true;
                }
                if let Some(style) = dom.attr(id, "style") {
                    // (?:^|;)\s*vertical-align\s*:\s*(?:super|sub)\b
                    for part in style.split(';') {
                        let part = part.trim().to_lowercase();
                        if let Some(value) = part.strip_prefix("vertical-align") {
                            let value = value.trim().strip_prefix(':').unwrap_or("").trim();
                            if value == "super"
                                || value == "sub"
                                || value.starts_with("super ")
                                || value.starts_with("sub ")
                            {
                                return true;
                            }
                        }
                    }
                }
                false
            }
            _ => false,
        }
    }
    let mut cursor = dom.parent(link);
    loop {
        if cursor == block {
            return if is_wrapper(dom, cursor) {
                Some(cursor)
            } else {
                None
            };
        }
        if is_wrapper(dom, cursor) {
            return Some(cursor);
        }
        if cursor == 0 {
            return None;
        }
        cursor = dom.parent(cursor);
    }
}

/// Annotation evidence check (oracle `_has_annotation_hint`).
fn has_annotation_hint(
    dom: &Dom,
    link: NodeId,
    marker: NodeId,
    marker_text: &str,
    allow_short_n: bool,
) -> bool {
    let decorated = marker_text
        .chars()
        .any(|c| !(c.is_ascii_digit() || c.is_whitespace() || matches!(c, '.' | '·' | ':' | '-')));
    let raw_href = dom.attr(link, "href").unwrap_or("");
    let fragment = raw_href.split('#').nth(1).unwrap_or("");
    let mut attrs = vec![fragment.to_string()];
    for node in [link, marker] {
        for key in ["id", "class", "role", "rel", "epub:type"] {
            if let Some(value) = dom.attr(node, key) {
                attrs.push(value.to_string());
            }
        }
    }
    let hint = attrs.join(" ");
    // Numbered note fragment: (notes?|footnotes?|endnotes?|fn)[-_ ]?\d+$ on fragment.
    let frag_lower = fragment.to_lowercase();
    let numbered = [
        "notes",
        "note",
        "footnotes",
        "footnote",
        "endnotes",
        "endnote",
        "fn",
    ]
    .into_iter()
    .any(|word| {
        frag_lower.len() >= word.len()
            && frag_lower[frag_lower.len() - word.len()..].starts_with(word)
            && {
                let rest = &frag_lower[..frag_lower.len() - word.len()];
                rest.is_empty() || rest.ends_with(['-', '_']) || {
                    let tail = &frag_lower[frag_lower.len() - word.len() + word.len()..];
                    tail.chars().all(|c| c.is_ascii_digit())
                }
            }
    }) && frag_lower
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .count()
        > 0;
    let short_n = allow_short_n && is_short_note_identity(fragment);
    decorated || hint_matches(&hint) || numbered || short_n
}

/// Trailing sup/sub note marker (oracle `_range_marker_node`).
fn range_marker_node(dom: &Dom, link: NodeId) -> Option<NodeId> {
    let significant: Vec<NodeId> = dom
        .children(link)
        .into_iter()
        .filter(|c| !(dom.is_text(*c) && dom.text_content(*c).trim().is_empty()))
        .collect();
    let candidate = *significant.last()?;
    if !matches!(dom.tag(candidate), Some("sup" | "sub")) {
        return None;
    }
    let marker_text = dom.get_text(candidate, "", true);
    if marker_text.is_empty() || !is_marker_only(&marker_text) {
        return None;
    }
    if dom.tag(candidate) == Some("sub") {
        let decorated = marker_text.chars().any(|c| {
            !(c.is_ascii_digit() || c.is_whitespace() || matches!(c, '.' | '·' | ':' | '-'))
        });
        if !decorated {
            return None;
        }
    }
    if has_annotation_hint(dom, link, candidate, &marker_text, true) {
        Some(candidate)
    } else {
        None
    }
}

/// Link body text excluding a confirmed marker (oracle `_semantic_link_text`).
fn semantic_link_text(dom: &Dom, link: NodeId, marker: Option<NodeId>) -> String {
    fn collect(dom: &Dom, id: NodeId, marker: Option<NodeId>, out: &mut String) {
        for child in dom.children(id) {
            if Some(child) == marker {
                continue;
            }
            if matches!(dom.tag(child), Some("rt" | "rp")) {
                continue;
            }
            if dom.is_text(child) {
                out.push_str(&dom.text_content(child));
            } else {
                collect(dom, child, marker, out);
            }
        }
    }
    let mut raw = String::new();
    collect(dom, link, marker, &mut raw);
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Relation classification (oracle `_annotation_relation`).
fn annotation_relation(
    dom: &Dom,
    link: NodeId,
    block: NodeId,
    marker_wrapper: Option<NodeId>,
    range_marker: Option<NodeId>,
    marker_only: bool,
) -> String {
    let semantics = semantic_tokens(dom, link);
    if semantics.contains("backlink") || semantics.contains("doc-backlink") {
        return "backlink".to_string();
    }
    if semantics.contains("noteref") || semantics.contains("doc-noteref") {
        return "noteref".to_string();
    }
    if inside_note_body(dom, link) {
        return "backlink".to_string();
    }
    if marker_only && inside_implicit_note_body(dom, link) {
        return "backlink".to_string();
    }
    if marker_only {
        let text = dom.get_text(link, "", true);
        if !text.is_empty()
            && text
                .chars()
                .all(|c| matches!(c, '↩' | '↵' | '←' | '↑' | '↓' | '⤶') || c.is_whitespace())
        {
            return "backlink".to_string();
        }
    }
    if marker_wrapper.is_none() && marker_only && link != block {
        let (before, after) = surrounding_text(dom, block, link);
        if before.trim().is_empty() && !after.trim().is_empty() {
            return "backlink".to_string();
        }
    }
    if marker_wrapper.is_some() || range_marker.is_some() || marker_only {
        return "noteref".to_string();
    }
    "internal_link".to_string()
}

/// Visible text before/after a node within a block (oracle `_surrounding_text`).
fn surrounding_text(dom: &Dom, block: NodeId, node: NodeId) -> (String, String) {
    fn text_of(dom: &Dom, id: NodeId) -> String {
        if dom.is_text(id) {
            dom.text_content(id)
        } else if dom.tag(id).is_some() {
            dom.get_text(id, " ", false)
        } else {
            String::new()
        }
    }
    let mut before = String::new();
    let mut after = String::new();
    let mut cursor = node;
    while cursor != block && cursor != 0 {
        for sibling in dom.previous_siblings(cursor) {
            before.push_str(&text_of(dom, sibling));
        }
        for sibling in dom.next_siblings(cursor) {
            after.push_str(&text_of(dom, sibling));
        }
        cursor = dom.parent(cursor);
    }
    (before, after)
}

/// Temporary annotation extraction spec for one structural root.
#[derive(Debug, Clone)]
struct Annotation {
    id: String,
    mode: String,
    marker_text: String,
    raw_href: String,
    target_key: String,
    relation: String,
    marker_node_ids: BTreeSet<NodeId>,
    root: NodeId,
}

/// Identify annotation roots within a block (oracle `_annotation_roots`).
fn annotation_roots(
    dom: &mut Dom,
    block: NodeId,
    anchor: &str,
    resource_href: &str,
) -> BTreeMap<NodeId, Annotation> {
    let mut links: Vec<NodeId> = Vec::new();
    if dom.tag(block) == Some("a")
        && dom.has_attr(block, "href")
        && dom
            .find_descendant(block, |d| matches!(dom.tag(d), Some("sup" | "sub")))
            .is_some()
    {
        links.push(block);
    }
    links.extend(dom.find_all_descendants(block, |d| {
        dom.tag(d) == Some("a") && dom.has_attr(d, "href")
    }));

    let mut roots: BTreeMap<NodeId, Annotation> = BTreeMap::new();
    let mut ordinal = 0usize;
    for link in links {
        if !is_internal_link(dom, link) {
            continue;
        }
        let mut marker_wrapper = nearest_marker_wrapper(dom, link, block);
        if let Some(wrapper) = marker_wrapper {
            let wrapper_text = dom.get_text(wrapper, "", true);
            if !has_annotation_hint(dom, link, wrapper, &wrapper_text, true) {
                marker_wrapper = None;
            }
        }
        let range_marker = if marker_wrapper.is_some() {
            None
        } else {
            range_marker_node(dom, link)
        };
        let semantic_text = semantic_link_text(dom, link, range_marker);
        if semantic_text.is_empty() && marker_wrapper.is_none() {
            continue;
        }
        let marker_shaped = is_marker_only(&semantic_text);
        let marker_only = marker_shaped
            && (has_annotation_hint(dom, link, link, &semantic_text, false)
                || inside_implicit_note_body(dom, link));
        let mode = if marker_wrapper.is_some() || marker_only {
            "point"
        } else {
            "range"
        };
        let root = if mode == "point" {
            marker_wrapper.unwrap_or(link)
        } else {
            link
        };
        let raw_href = dom.attr(link, "href").unwrap_or("").to_string();
        let resolved = resolve_epub_href(resource_href, &raw_href);
        let relation =
            annotation_relation(dom, link, block, marker_wrapper, range_marker, marker_only);
        if roots.contains_key(&root) {
            continue;
        }
        let annotation_id = format!("{anchor}_annotation_{ordinal}");
        ordinal += 1;
        let marker_text = if mode == "point" {
            dom.get_text(root, "", true)
        } else if let Some(marker) = range_marker {
            dom.get_text(marker, "", true)
        } else {
            String::new()
        };
        dom.set_attr(root, ANNOTATION_ID_ATTR, &annotation_id);
        let mut marker_node_ids = BTreeSet::new();
        if let Some(marker) = range_marker {
            marker_node_ids.insert(marker);
        }
        roots.insert(
            root,
            Annotation {
                id: annotation_id,
                mode: mode.to_string(),
                marker_text,
                raw_href,
                target_key: resolved.target_key(),
                relation,
                marker_node_ids,
                root,
            },
        );
    }
    roots
}

/// Preserved inline roots (oracle `_preserved_inline_roots`).
fn preserved_inline_roots(dom: &Dom, block: NodeId) -> Vec<NodeId> {
    let mut roots = Vec::new();
    let mut seen = BTreeSet::new();
    for candidate in dom.find_all_descendants(block, |_| true) {
        if dom.has_attr(candidate, ANNOTATION_ID_ATTR) {
            continue;
        }
        let is_atomic = dom
            .tag(candidate)
            .is_some_and(|t| ATOMIC_INLINE_TAGS.contains(&t));
        let is_empty_anchor = matches!(dom.tag(candidate), Some("a" | "span"))
            && dom.text_content(candidate).trim().is_empty()
            && (dom.has_attr(candidate, "id") || dom.has_attr(candidate, "name"));
        if !is_atomic && !is_empty_anchor {
            continue;
        }
        let mut root = candidate;
        let mut parent = dom.parent(root);
        while parent != block
            && dom.tag(parent).is_some()
            && !STRICT_BLOCK_TAGS.contains(&dom.tag(parent).unwrap_or(""))
            && !dom.has_attr(parent, ANNOTATION_ID_ATTR)
            && dom.text_content(parent).trim().is_empty()
        {
            root = parent;
            parent = dom.parent(root);
        }
        if seen.insert(root) {
            roots.push(root);
        }
    }
    roots
}

/// Collapse HTML whitespace with offset mapping (oracle `_normalize_html_text`).
fn normalize_html_text(raw: &str, offsets: &[usize]) -> (String, Vec<usize>) {
    let chars: Vec<char> = raw.chars().collect();
    let mut boundary_map = vec![0usize; chars.len() + 1];
    let mut output: Vec<char> = Vec::new();
    for (index, ch) in chars.iter().enumerate() {
        boundary_map[index] = output.len();
        if matches!(ch, ' ' | '\t' | '\r' | '\n' | '\u{c}' | '\u{b}') {
            if output.last() != Some(&' ') {
                output.push(' ');
            }
        } else {
            output.push(*ch);
        }
    }
    boundary_map[chars.len()] = output.len();
    let collapsed: String = output.into_iter().collect();
    let leading = collapsed.len() - collapsed.trim_start().len();
    let text = collapsed.trim().to_string();
    let mapped = offsets
        .iter()
        .map(|offset| {
            let clamped = (*offset).min(chars.len());
            (boundary_map[clamped].saturating_sub(leading)).min(text.len())
        })
        .collect();
    (text, mapped)
}

/// Extract segment text + inline/annotation meta (oracle `_segment_content`).
#[allow(clippy::too_many_arguments)]
fn segment_content(
    dom: &mut Dom,
    block: NodeId,
    anchor: &str,
    annotations: &BTreeMap<NodeId, Annotation>,
) -> (String, serde_json::Map<String, serde_json::Value>) {
    let mut marker_node_ids = BTreeSet::new();
    for annotation in annotations.values() {
        marker_node_ids.extend(annotation.marker_node_ids.iter().copied());
    }
    let roots = preserved_inline_roots(dom, block);
    let root_ids: BTreeSet<NodeId> = roots.iter().copied().collect();
    let mut text_parts: Vec<String> = Vec::new();
    let mut preserved: Vec<(NodeId, usize)> = Vec::new();
    let mut annotation_events: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut raw_length = 0usize;

    fn walk(
        dom: &Dom,
        id: NodeId,
        inside_range: bool,
        annotations: &BTreeMap<NodeId, Annotation>,
        marker_node_ids: &BTreeSet<NodeId>,
        root_ids: &BTreeSet<NodeId>,
        text_parts: &mut Vec<String>,
        preserved: &mut Vec<(NodeId, usize)>,
        annotation_events: &mut BTreeMap<String, (usize, usize)>,
        raw_length: &mut usize,
    ) {
        for child in dom.children(id) {
            if dom.tag(child).is_some() {
                let tag = dom.tag(child).unwrap_or("");
                if tag == "ruby" {
                    let base_start = *raw_length;
                    walk(
                        dom,
                        child,
                        inside_range,
                        annotations,
                        marker_node_ids,
                        root_ids,
                        text_parts,
                        preserved,
                        annotation_events,
                        raw_length,
                    );
                    let reading: String = dom
                        .find_all_descendants(child, |d| dom.tag(d) == Some("rt"))
                        .into_iter()
                        .map(|rt| dom.text_content(rt))
                        .collect::<Vec<_>>()
                        .join("")
                        .trim()
                        .to_string();
                    if !reading.is_empty() && *raw_length > base_start {
                        text_parts.push(format!("{RUBY_MARK_LEFT}{reading}{RUBY_MARK_RIGHT}"));
                        *raw_length += reading.chars().count() + 2;
                    }
                    continue;
                }
                if tag == "rt" || tag == "rp" {
                    continue;
                }
                if inside_range && marker_node_ids.contains(&child) {
                    continue;
                }
                if let Some(annotation) = annotations.get(&child) {
                    let start = *raw_length;
                    if annotation.mode == "range" {
                        walk(
                            dom,
                            child,
                            true,
                            annotations,
                            marker_node_ids,
                            root_ids,
                            text_parts,
                            preserved,
                            annotation_events,
                            raw_length,
                        );
                    }
                    annotation_events.insert(annotation.id.clone(), (start, *raw_length));
                }
                if root_ids.contains(&child) {
                    preserved.push((child, *raw_length));
                } else if annotations.contains_key(&child) {
                    continue;
                } else {
                    walk(
                        dom,
                        child,
                        inside_range,
                        annotations,
                        marker_node_ids,
                        root_ids,
                        text_parts,
                        preserved,
                        annotation_events,
                        raw_length,
                    );
                }
            } else if dom.is_text(child) {
                let mut value = dom.text_content(child);
                let next_siblings = dom.next_siblings(child);
                if inside_range
                    && next_siblings
                        .first()
                        .is_some_and(|n| marker_node_ids.contains(n))
                    && dom.tag(next_siblings[0]).is_some()
                {
                    value = value
                        .trim_end_matches([' ', '\t', '\r', '\n', '\u{c}', '\u{b}'])
                        .to_string();
                }
                if inside_range && value.trim().is_empty() {
                    let previous = dom.previous_siblings(child).into_iter().next();
                    let has_later_text = dom.next_siblings(child).into_iter().any(|s| {
                        if dom.tag(s).is_some() {
                            !dom.text_content(s).trim().is_empty()
                        } else {
                            dom.is_text(s) && !dom.text_content(s).trim().is_empty()
                        }
                    });
                    if previous.is_some_and(|p| marker_node_ids.contains(&p)) && !has_later_text {
                        continue;
                    }
                }
                *raw_length += value.chars().count();
                text_parts.push(value);
            }
        }
    }

    if let Some(block_annotation) = annotations.get(&block).cloned() {
        if block_annotation.mode == "range" {
            walk(
                dom,
                block,
                true,
                annotations,
                &marker_node_ids,
                &root_ids,
                &mut text_parts,
                &mut preserved,
                &mut annotation_events,
                &mut raw_length,
            );
        }
        annotation_events.insert(block_annotation.id.clone(), (0, raw_length));
    } else {
        walk(
            dom,
            block,
            false,
            annotations,
            &marker_node_ids,
            &root_ids,
            &mut text_parts,
            &mut preserved,
            &mut annotation_events,
            &mut raw_length,
        );
    }

    let raw_text: String = text_parts.concat();
    // Offsets are char-based in the oracle (Python str indices); raw_length too.
    let mut event_offsets: Vec<usize> = preserved.iter().map(|(_, o)| *o).collect();
    let ordered: Vec<&Annotation> = annotations.values().collect();
    for annotation in &ordered {
        let (start, end) = annotation_events
            .get(&annotation.id)
            .copied()
            .unwrap_or((raw_length, raw_length));
        event_offsets.extend([start, end]);
    }
    let (text, normalized_offsets) = normalize_html_text(&raw_text, &event_offsets);
    if text.is_empty() {
        return (String::new(), serde_json::Map::new());
    }

    let source_length = text.chars().count();
    let mut nodes = Vec::new();
    let mut cursor = 0;
    for (index, (node, _)) in preserved.iter().enumerate() {
        let inline_id = format!("{anchor}_inline_{index}");
        let offset = normalized_offsets[cursor];
        cursor += 1;
        let placement = if offset == 0 {
            "before"
        } else if offset == source_length {
            "after"
        } else {
            "inline"
        };
        dom.set_attr(*node, INLINE_ID_ATTR, &inline_id);
        nodes.push(serde_json::json!({
            "id": inline_id,
            "tag": dom.tag(*node).unwrap_or(""),
            "placement": placement,
            "offset": offset,
        }));
    }

    let mut meta = serde_json::Map::new();
    if !nodes.is_empty() {
        meta.insert(
            INLINE_META_KEY.to_string(),
            serde_json::json!({
                "version": 1,
                "source_length": source_length,
                "nodes": nodes,
            }),
        );
    }
    let mut items = Vec::new();
    for annotation in &ordered {
        let start = normalized_offsets[cursor];
        let end = normalized_offsets[cursor + 1];
        cursor += 2;
        let source_text: String = text
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect();
        items.push(serde_json::json!({
            "id": annotation.id,
            "mode": annotation.mode,
            "source_start": start,
            "source_end": end,
            "source_text": source_text,
            "marker_text": annotation.marker_text,
            "raw_href": annotation.raw_href,
            "target_key": annotation.target_key,
            "relation": annotation.relation,
        }));
    }
    if !items.is_empty() {
        meta.insert(
            ANNOTATION_META_KEY.to_string(),
            serde_json::json!({
                "version": 1,
                "source_length": source_length,
                "items": items,
            }),
        );
    }
    (text, meta)
}

/// Whether an element has a meaningful descendant block (oracle helper).
fn has_meaningful_descendant_block(dom: &Dom, id: NodeId) -> bool {
    dom.find_all_descendants(id, |d| dom.tag(d).is_some_and(|t| BLOCK_TAGS.contains(&t)))
        .into_iter()
        .any(|d| !dom.text_content(d).trim().is_empty())
}

/// Direct link that is a list item's only body text (oracle `_list_item_link_target`).
fn list_item_link_target(dom: &Dom, id: NodeId) -> Option<NodeId> {
    let link = dom
        .children(id)
        .into_iter()
        .find(|c| dom.tag(*c) == Some("a"))?;
    if dom.text_content(link).trim().is_empty() {
        return None;
    }
    for child in dom.children(id) {
        if child == link {
            continue;
        }
        if let Some(tag) = dom.tag(child) {
            if BLOCK_TAGS.contains(&tag) || matches!(tag, "ul" | "ol" | "dl") {
                continue;
            }
            if !dom.text_content(child).trim().is_empty() {
                return None;
            }
            continue;
        }
        if dom.is_text(child) && !dom.text_content(child).trim().is_empty() {
            return None;
        }
    }
    Some(link)
}

/// Wrap br-separated runs as independent targets (oracle `_split_direct_break_lines`).
fn split_direct_break_lines(dom: &mut Dom, id: NodeId) -> Vec<NodeId> {
    let children = dom.children(id);
    if !children.iter().any(|c| dom.tag(*c) == Some("br")) {
        return vec![id];
    }
    let mut runs: Vec<Vec<NodeId>> = vec![Vec::new()];
    for child in children {
        if dom.tag(child) == Some("br") {
            runs.push(Vec::new());
        } else if dom.tag(child).is_some() || dom.is_text(child) {
            runs.last_mut().unwrap().push(child);
        }
    }
    let mut targets = Vec::new();
    for run in runs {
        let has_text = run.iter().any(|n| {
            if dom.tag(*n).is_some() {
                !dom.text_content(*n).trim().is_empty()
            } else {
                dom.is_text(*n) && !dom.text_content(*n).trim().is_empty()
            }
        });
        if !has_text || run.is_empty() {
            continue;
        }
        // Create wrapper span before the first run node, then move run nodes in.
        let wrapper = dom.insert_before(
            run[0],
            crate::document::markup::Node::Element {
                tag: "span".to_string(),
                attrs: vec![(LINE_WRAPPER_ATTR.to_string(), "true".to_string())],
                children: Vec::new(),
            },
        );
        for node in run {
            dom.move_to(node, wrapper);
        }
        targets.push(wrapper);
    }
    targets
}

/// Whether a block sits inside an EPUB3 nav TOC list (oracle helper).
fn inside_navigation_list(dom: &Dom, id: NodeId) -> bool {
    let mut inside_nav = false;
    let mut inside_item = dom.tag(id) == Some("li");
    for parent in dom.ancestors(id) {
        match dom.tag(parent) {
            Some("li") => inside_item = true,
            Some("nav") => {
                inside_nav = true;
                break;
            }
            _ => {}
        }
    }
    inside_nav && inside_item
}

/// Select finest replaceable nodes in document order (oracle `_translation_targets`).
fn translation_targets(dom: &mut Dom, skip_navigation: bool) -> Vec<NodeId> {
    // Collect candidates first (document order), then split br-lines (mutates).
    let candidates: Vec<NodeId> = dom
        .descendants(0)
        .into_iter()
        .filter(|d| dom.tag(*d).is_some_and(|t| BLOCK_TAGS.contains(&t)))
        .collect();
    let mut targets = Vec::new();
    for element in candidates {
        if skip_navigation && inside_navigation_list(dom, element) {
            continue;
        }
        let has_descendant_block = has_meaningful_descendant_block(dom, element);
        if dom.tag(element) == Some("li") {
            let link = list_item_link_target(dom, element);
            if let Some(link) = link {
                targets.extend(split_direct_break_lines(dom, link));
            }
            if link.is_some() || has_descendant_block {
                continue;
            }
        }
        if has_descendant_block {
            continue;
        }
        targets.extend(split_direct_break_lines(dom, element));
    }
    targets
}

/// Internal filename/book-title check (oracle `_looks_like_internal_title`).
fn looks_like_internal_title(title: &str, href: &str, book_title: &str) -> bool {
    let base = href
        .rsplit('/')
        .next()
        .unwrap_or("")
        .rsplit('.')
        .nth(1)
        .unwrap_or("");
    // Oracle: basename minus last extension; handle extensionless names.
    let base = if href.rsplit('/').next().unwrap_or("").contains('.') {
        base
    } else {
        href.rsplit('/').next().unwrap_or("")
    };
    let stripped = title.trim();
    (!base.is_empty() && stripped == base)
        || (!book_title.is_empty() && stripped == book_title.trim())
}

/// Hoist processing instructions before the block (oracle helper).
fn hoist_processing_instructions(dom: &mut Dom, id: NodeId) {
    let pis: Vec<NodeId> = dom
        .descendants(id)
        .into_iter()
        .filter(|d| dom.is_pi(*d))
        .collect();
    for pi in pis {
        dom.detach(pi);
        // Re-insert before the block preserving relative order: collect then insert.
        let parent = dom.parent(id);
        // insert_before with a fresh node won't work for moves; use move_to + reorder.
        dom.move_to(pi, parent);
        // Move it just before `id`: fix order by rotating.
        let siblings = dom.children(parent);
        if let (Some(pi_pos), Some(id_pos)) = (
            siblings.iter().position(|s| *s == pi),
            siblings.iter().position(|s| *s == id),
        ) {
            if pi_pos > id_pos {
                // Rebuild child order: move pi before id.
                if let crate::document::markup::Node::Element { children, .. }
                | crate::document::markup::Node::Root { children } = &mut dom.nodes[parent]
                {
                    children.remove(pi_pos);
                    let id_pos = children.iter().position(|s| *s == id).unwrap_or(0);
                    children.insert(id_pos, pi);
                }
            }
        }
    }
}

/// Annotate one physical XHTML resource (oracle `annotate_epub_resource`).
/// Returns (title, segments, template).
pub fn annotate_resource(
    html: &str,
    resource_index: usize,
    href: &str,
    book_title: &str,
    skip_navigation: bool,
) -> (String, Vec<Segment>, String) {
    let mut dom = parse_html(html);
    let mut segments = Vec::new();
    let mut first_heading: Option<NodeId> = None;
    let mut heading_title_parts: Vec<String> = Vec::new();
    let mut idx = 0usize;

    for el in translation_targets(&mut dom, skip_navigation) {
        let anchor = format!("tn{resource_index}_{idx}");
        let annotations = annotation_roots(&mut dom, el, &anchor, href);
        let mut protected: BTreeSet<NodeId> = BTreeSet::new();
        let mut range_roots: BTreeSet<NodeId> = BTreeSet::new();
        for annotation in annotations.values() {
            protected.insert(annotation.root);
            if annotation.mode == "point" {
                protected.extend(dom.descendants(annotation.root));
            } else {
                range_roots.insert(annotation.root);
                for node in dom.descendants(annotation.root) {
                    if annotation.marker_node_ids.contains(&node)
                        || dom.ancestors(node).into_iter().any(|p| {
                            p != annotation.root && annotation.marker_node_ids.contains(&p)
                        })
                    {
                        protected.insert(node);
                    }
                }
            }
        }
        // Flatten text-bearing id/name wrappers into empty anchors.
        let descendants = dom.descendants(el);
        for descendant in descendants {
            if dom.text_content(descendant).trim().is_empty() {
                continue;
            }
            if protected.contains(&descendant) {
                continue;
            }
            let mut anchor_attrs = Vec::new();
            for key in ["id", "name"] {
                if let Some(value) = dom.remove_attr(descendant, key) {
                    anchor_attrs.push((key.to_string(), value));
                }
            }
            if anchor_attrs.is_empty() {
                continue;
            }
            let inside_range = dom
                .ancestors(descendant)
                .into_iter()
                .any(|p| range_roots.contains(&p));
            let marker = dom.insert_before(
                descendant,
                crate::document::markup::Node::Element {
                    tag: if inside_range {
                        "span".to_string()
                    } else {
                        "a".to_string()
                    },
                    attrs: anchor_attrs,
                    children: Vec::new(),
                },
            );
            let _ = marker;
        }

        let (text, meta) = segment_content(&mut dom, el, &anchor, &annotations);
        if !annotations.is_empty()
            && annotations.values().all(|a| a.mode == "point")
            && is_marker_only(&text)
        {
            continue;
        }
        if text.is_empty() {
            continue;
        }
        dom.set_attr(el, SEGMENT_ID_ATTR, &anchor);
        hoist_processing_instructions(&mut dom, el);
        let is_heading = dom.tag(el).is_some_and(|t| HEADING_TAGS.contains(&t))
            || dom
                .find_parent(el, |p| {
                    dom.tag(p).is_some_and(|t| HEADING_TAGS.contains(&t))
                })
                .is_some();
        let kind = if is_heading { KIND_HEADING } else { KIND_TEXT };
        if is_heading {
            let heading = if dom.tag(el).is_some_and(|t| HEADING_TAGS.contains(&t)) {
                Some(el)
            } else {
                dom.find_parent(el, |p| {
                    dom.tag(p).is_some_and(|t| HEADING_TAGS.contains(&t))
                })
            };
            if let Some(heading) = heading {
                if first_heading.is_none() {
                    first_heading = Some(heading);
                }
                if first_heading == Some(heading) {
                    heading_title_parts.push(text.clone());
                }
            }
        }
        segments.push(Segment {
            index: idx,
            source: text,
            kind: kind.to_string(),
            target: None,
            target_before_polish: None,
            anchor: Some(anchor),
            resource_href: Some(href.to_string()),
            cont: false,
            meta,
        });
        idx += 1;
    }

    let mut title = heading_title_parts.join(" ");
    if title.is_empty() {
        // First <title> element with non-empty string.
        if let Some(title_el) = dom
            .descendants(0)
            .into_iter()
            .find(|d| dom.tag(*d) == Some("title"))
        {
            let candidate = dom.text_content(title_el).trim().to_string();
            // Oracle uses soup.title.string (direct string only); approximate
            // with full text (equal for plain <title> elements).
            if !candidate.is_empty() && !looks_like_internal_title(&candidate, href, book_title) {
                title = candidate;
            }
        }
    }
    (title, segments, dom.serialize())
}

/// Strip ruby reading markers (oracle `strip_ruby_markers`).
pub fn strip_ruby_markers(text: &str) -> String {
    super::ruby::strip_markers(text)
}
