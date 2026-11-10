//! Single-file HTML reader. Oracle: `ingest/html_reader.py`.
//!
//! Splits HTML by headings into chapters, then annotates block segments with
//! the EPUB annotator (`tn{chapter}_{segment}` anchors). Each chapter keeps
//! its annotated template for backfill.

use std::path::Path;

use crate::document::annotate::annotate_resource;
use crate::document::markup::{parse_html, Dom, Node, NodeId};
use crate::document::models::{Chapter, Document};
use crate::{Error, Result};

/// Tags that start chapters by default (all heading levels).
const DEFAULT_CHAPTER_TAGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];

/// Media tags that preserve an otherwise text-free chapter (oracle parity).
const VISIBLE_MEDIA_TAGS: [&str; 8] = [
    "img", "picture", "svg", "object", "video", "audio", "canvas", "iframe",
];

/// Whether a node is a comment.
fn is_comment(dom: &Dom, id: NodeId) -> bool {
    matches!(dom.nodes[id], Node::Comment { .. })
}

/// Whether a node is non-whitespace text.
fn is_nonempty_text(dom: &Dom, id: NodeId) -> bool {
    match &dom.nodes[id] {
        Node::Text { text } => !text.trim().is_empty(),
        _ => false,
    }
}

/// Find the first `body`, else the document root.
fn body_or_root(dom: &Dom) -> NodeId {
    dom.find_descendant(0, |d| dom.tag(d) == Some("body"))
        .unwrap_or(0)
}

/// Serialize one child for a chapter fragment (oracle comment re-wrap).
fn serialize_child(dom: &Dom, id: NodeId) -> String {
    match &dom.nodes[id] {
        Node::Comment { body } => format!("<!--{body}-->"),
        _ => dom.serialize_node_str(id),
    }
}

/// Parse one HTML file into a `Document` (oracle `read_html`).
///
/// `chapter_tags`: boundary tags starting chapters (`None` = one chapter for
/// the whole document). Default is all heading levels.
pub fn read_html(
    path: &Path,
    source_lang: &str,
    target_lang: &str,
    chapter_tags: Option<&[&str]>,
) -> Result<Document> {
    let raw = std::fs::read(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let dom = parse_html(&text);
    let book_title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let head_html = dom
        .find_descendant(0, |d| dom.tag(d) == Some("head"))
        .map(|head| dom.serialize_contents(head))
        .unwrap_or_default();
    let body = body_or_root(&dom);
    let children = dom.children(body);
    // Oracle default splits on all heading levels; `chapter_tags=None`
    // disables splitting. Rust spelling: `None` = default headings,
    // `Some(&[])` = one chapter for the whole document.
    let split_tags: &[&str] = match chapter_tags {
        None => &DEFAULT_CHAPTER_TAGS,
        Some(tags) => tags,
    };

    // Boundaries: first heading of each consecutive run starts a chapter.
    let mut boundaries: Vec<usize> = Vec::new();
    if !split_tags.is_empty() {
        for (i, child) in children.iter().enumerate() {
            let is_boundary = dom.tag(*child).is_some_and(|t| split_tags.contains(&t));
            if !is_boundary {
                continue;
            }
            let mut prev_is_heading = false;
            for prev in children[..i].iter().rev() {
                if dom.tag(*prev).is_some() {
                    prev_is_heading = dom.tag(*prev).is_some_and(|t| split_tags.contains(&t));
                    break;
                }
                if is_nonempty_text(&dom, *prev) {
                    break;
                }
            }
            if !prev_is_heading {
                boundaries.push(i);
            }
        }
    }

    let mut intervals: Vec<(usize, usize)> = Vec::new();
    if boundaries.is_empty() {
        intervals.push((0, children.len()));
    } else {
        if boundaries[0] > 0 {
            intervals.push((0, boundaries[0]));
        }
        for (bi, start) in boundaries.iter().enumerate() {
            let end = boundaries.get(bi + 1).copied().unwrap_or(children.len());
            intervals.push((*start, end));
        }
    }

    let mut chapters: Vec<Chapter> = Vec::new();
    for (ci, (start, end)) in intervals.iter().enumerate() {
        let slice = &children[*start..*end];
        let fragment: String = slice.iter().map(|c| serialize_child(&dom, *c)).collect();
        // Chapter title: consecutive leading headings joined with " / ".
        let mut chapter_title = String::new();
        if let Some(first) = slice.first() {
            if dom
                .tag(*first)
                .is_some_and(|t| DEFAULT_CHAPTER_TAGS.contains(&t))
            {
                let mut titles = Vec::new();
                for child in slice {
                    if let Some(tag) = dom.tag(*child) {
                        if DEFAULT_CHAPTER_TAGS.contains(&tag) {
                            let normalized = dom
                                .text_content(*child)
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ");
                            titles.push(normalized);
                        } else {
                            break;
                        }
                    } else if is_nonempty_text(&dom, *child)
                        || !matches!(dom.nodes[*child], Node::Text { .. })
                            && !is_comment(&dom, *child)
                    {
                        break;
                    }
                }
                chapter_title = titles.join(" / ");
            }
        }
        let href = format!("html-chapter-{ci}.xhtml");
        // Oracle passes no book_title ("" default) and no skip_navigation.
        let (detected_title, mut segments, template) =
            annotate_resource(&fragment, ci, &href, "", false);
        for segment in &mut segments {
            segment.resource_href = None;
        }
        let title = if chapter_title.trim().is_empty() {
            detected_title
        } else {
            chapter_title.trim().to_string()
        };
        let template_dom = parse_html(&template);
        let has_media = template_dom
            .find_descendant(0, |d| {
                template_dom
                    .tag(d)
                    .is_some_and(|t| VISIBLE_MEDIA_TAGS.contains(&t))
            })
            .is_some();
        if !segments.iter().any(|s| !s.source.trim().is_empty()) && !has_media {
            continue;
        }
        chapters.push(Chapter {
            index: chapters.len(),
            title,
            segments,
            href: None,
            template: Some(template),
            meta: serde_json::Map::new(),
        });
    }

    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut meta = serde_json::Map::new();
    meta.insert(
        "chapter_tags".to_string(),
        if split_tags.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::Array(
                split_tags
                    .iter()
                    .map(|t| serde_json::Value::String(t.to_string()))
                    .collect(),
            )
        },
    );
    meta.insert(
        "head_html".to_string(),
        serde_json::Value::String(head_html),
    );
    Ok(Document {
        title: book_title,
        source_lang: source_lang.to_string(),
        target_lang: target_lang.to_string(),
        fmt: "html".to_string(),
        source_path: absolute.to_string_lossy().into_owned(),
        chapters,
        meta,
    })
}
