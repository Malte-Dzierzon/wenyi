//! EPUB package reading: container, OPF, TOC entries.
//!
//! Oracle: `ingest/epub_package.py` + `ingest/epub_toc.py`. Namespace-insensitive
//! local-name matching throughout, like the oracle's `rsplit("}", 1)[-1]`.

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use quick_xml::XmlVersion;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use zip::ZipArchive;

use crate::document::anchors::resolve_epub_href;
use crate::{Error, Result};

const CONTAINER: &str = "META-INF/container.xml";

/// Local tag name (strip any `prefix:` qualifier).
fn local_name(name: quick_xml::name::QName<'_>) -> &str {
    name.into_inner().rsplit(':').next().unwrap_or("")
}

/// Attribute value lookup by local name.
fn attr_value(element: &quick_xml::events::BytesStart, key: &str) -> String {
    element
        .attributes()
        .flatten()
        .find(|a| local_name(a.key) == key)
        .and_then(|a| a.normalized_value(XmlVersion::Implicit1_0).ok())
        .map(|v| v.into_owned())
        .unwrap_or_default()
}

fn read_member(archive: &mut ZipArchive<File>, name: &str) -> Result<Vec<u8>> {
    let mut file = archive
        .by_name(name)
        .map_err(|e| Error::Config(format!("Invalid EPUB: missing zip member {name:?}: {e}")))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|e| Error::Io {
        path: Path::new(name).to_path_buf(),
        message: e.to_string(),
    })?;
    Ok(buf)
}

/// Decode markup honoring XML declarations; UTF-8 replacement as fallback.
/// (Oracle uses BeautifulSoup UnicodeDammit; UTF-8 + BOM handling covers the
/// fixture corpus. Non-UTF-8 legacy encodings without BOM fall back the same way.)
pub fn decode_markup(data: &[u8]) -> String {
    // Strip UTF-8 BOM.
    let data = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
    // Honor encoding="..." declarations for the common cases.
    let head = &data[..data.len().min(512)];
    let head_text = String::from_utf8_lossy(head);
    let declared = head_text
        .find("encoding=")
        .and_then(|pos| {
            let rest = &head_text[pos + 9..];
            let quote = rest.chars().next()?;
            if quote == '"' || quote == '\'' {
                rest[1..]
                    .find(quote)
                    .map(|end| rest[1..1 + end].to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
        .to_lowercase();
    if declared == "shift_jis" || declared == "shift-jis" || declared == "windows-31j" {
        return decode_shift_jis(data);
    }
    String::from_utf8_lossy(data).into_owned()
}

/// Minimal Shift_JIS decoder for legacy EPUBs (ASCII + half-width kana +
/// JIS X 0208 via a compact range mapping is out of scope; fall back to
/// replacement for unmapped pairs — matches oracle behavior of never failing).
fn decode_shift_jis(data: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if b < 0x80 {
            out.push(b as char);
            i += 1;
        } else if (0xa1..=0xdf).contains(&b) {
            // Half-width katakana U+FF61..U+FF9F.
            out.push(char::from_u32(0xff61 + (b - 0xa1) as u32).unwrap_or('�'));
            i += 1;
        } else if i + 1 < data.len() {
            // Double-byte sequence: oracle fidelity for these requires a full
            // JIS table; emit replacement (documented limitation, fixtures use UTF-8).
            out.push('�');
            i += 2;
        } else {
            out.push('�');
            i += 1;
        }
    }
    out
}

/// Resolve the OPF package path from `META-INF/container.xml`.
pub fn find_opf_path(archive: &mut ZipArchive<File>) -> Result<String> {
    let data = read_member(archive, CONTAINER)?;
    let mut reader = Reader::from_reader(data.as_slice());
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(element)) | Ok(Event::Start(element)) => {
                if local_name(element.name()) == "rootfile" {
                    let path = attr_value(&element, "full-path").trim().to_string();
                    if !path.is_empty() {
                        return Ok(path);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(Error::Config(format!("Invalid EPUB container.xml: {e}"))),
            _ => {}
        }
        buf.clear();
    }
    Err(Error::Config(
        "Invalid EPUB: container.xml has no valid rootfile full-path".to_string(),
    ))
}

/// Parsed OPF package: title, spine XHTML paths, TOC paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpfPackage {
    /// Book title (first non-empty dc:title).
    pub title: String,
    /// Spine-ordered XHTML zip paths (deduped).
    pub spine_hrefs: Vec<String>,
    /// TOC/NAV zip paths (NAV first, then spine-toc, then NCX).
    pub toc_paths: Vec<String>,
    /// All manifest XHTML paths (superset for annotation lookup).
    pub manifest_xhtml_hrefs: Vec<String>,
}

/// Parse the OPF package document (oracle `_parse_opf` + `_manifest_xhtml_hrefs`).
pub fn parse_opf(archive: &mut ZipArchive<File>, opf_path: &str) -> Result<OpfPackage> {
    let data = read_member(archive, opf_path)?;
    let text = decode_markup(&data);
    let mut reader = Reader::from_str(&text);
    reader.config_mut().trim_text(true);

    let mut title = String::new();
    // Document order matters (oracle dict insertion order); keep an ordered vec plus an id index.
    let mut manifest: Vec<(String, (String, String, String))> = Vec::new();
    let mut spine_ids: Vec<String> = Vec::new();
    let mut toc_ids: Vec<String> = Vec::new();
    let mut in_title = false;
    let mut buf = Vec::new();

    let attr = attr_value;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                let name = local_name(element.name());
                match name {
                    "title" => {
                        if title.is_empty() {
                            in_title = true;
                        }
                    }
                    "item" => {
                        let id = attr(&element, "id").trim().to_string();
                        if !id.is_empty() {
                            let entry = (
                                attr(&element, "href"),
                                attr(&element, "media-type"),
                                attr(&element, "properties"),
                            );
                            if let Some(slot) = manifest.iter_mut().find(|(mid, _)| *mid == id) {
                                slot.1 = entry;
                            } else {
                                manifest.push((id, entry));
                            }
                        }
                    }
                    "itemref" => {
                        let idref = attr(&element, "idref").trim().to_string();
                        if !idref.is_empty() {
                            spine_ids.push(idref);
                        }
                    }
                    "spine" => {
                        let toc = attr(&element, "toc");
                        if !toc.is_empty() {
                            toc_ids.push(toc);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(element)) => {
                if in_title {
                    let text = element.xml_content(XmlVersion::Implicit1_0);
                    if title.is_empty() && !text.trim().is_empty() {
                        title = text.trim().to_string();
                    }
                }
            }
            Ok(Event::End(element)) => {
                if local_name(element.name()) == "title" {
                    in_title = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(Error::Config(format!("Invalid EPUB OPF: {e}"))),
            _ => {}
        }
        buf.clear();
    }

    let zip_href = |href: &str| resolve_epub_href(opf_path, href).resource_href;
    let is_xhtml = |href: &str, media: &str| {
        media.contains("html")
            || href.ends_with(".xhtml")
            || href.ends_with(".html")
            || href.ends_with(".htm")
    };

    let mut spine_hrefs = Vec::new();
    for sid in &spine_ids {
        let Some((href, media, _)) = manifest.iter().find(|(mid, _)| mid == sid).map(|(_, v)| v)
        else {
            continue;
        };
        if !is_xhtml(href, media) {
            continue;
        }
        let resolved = zip_href(href);
        if !resolved.is_empty() && !spine_hrefs.contains(&resolved) {
            spine_hrefs.push(resolved);
        }
    }

    let nav_ids: Vec<&String> = manifest
        .iter()
        .filter(|(_, (_, _, props))| props.split_whitespace().any(|p| p == "nav"))
        .map(|(id, _)| id)
        .collect();
    let ncx_ids: Vec<&String> = manifest
        .iter()
        .filter(|(_, (_, media, _))| media == "application/x-dtbncx+xml")
        .map(|(id, _)| id)
        .collect();
    let mut toc_paths = Vec::new();
    for item_id in nav_ids.into_iter().chain(toc_ids.iter()).chain(ncx_ids) {
        let Some((href, _, _)) = manifest
            .iter()
            .find(|(mid, _)| mid == item_id)
            .map(|(_, v)| v)
        else {
            continue;
        };
        let resolved = zip_href(href);
        if !resolved.is_empty() && !toc_paths.contains(&resolved) {
            toc_paths.push(resolved);
        }
    }

    let mut manifest_xhtml_hrefs = Vec::new();
    for (_, (href, media, _)) in manifest.iter() {
        let media_lower = media.to_lowercase();
        let path_lower = href.split(['?', '#']).next().unwrap_or("").to_lowercase();
        if media_lower.contains("html")
            || path_lower.ends_with(".xhtml")
            || path_lower.ends_with(".html")
            || path_lower.ends_with(".htm")
        {
            let resolved = zip_href(href);
            if !resolved.is_empty() && !manifest_xhtml_hrefs.contains(&resolved) {
                manifest_xhtml_hrefs.push(resolved);
            }
        }
    }

    Ok(OpfPackage {
        title,
        spine_hrefs,
        toc_paths,
        manifest_xhtml_hrefs,
    })
}

/// One TOC entry (oracle `parse_toc_entries` dict shape).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TocEntry {
    /// `toc_path:node_index`.
    pub entry_id: String,
    /// TOC zip path.
    pub toc_path: String,
    /// Preorder node index within this TOC.
    pub node_index: usize,
    /// Source node id (NCX navPoint id or "").
    pub node_id: String,
    /// Parent preorder index (None for top level).
    pub parent_index: Option<usize>,
    /// Nesting depth.
    pub depth: usize,
    /// `ncx` or `nav`.
    pub kind: String,
    /// Entry title.
    pub title: String,
    /// Raw href as written.
    pub raw_href: String,
    /// Resolved zip member.
    pub resource_href: String,
    /// Percent-decoded fragment.
    pub fragment: String,
    /// `resource#fragment` destination key.
    pub target_key: String,
    /// True for remote URLs.
    pub external: bool,
}

/// Parse all TOC documents (oracle `parse_toc_entries`).
/// A damaged secondary TOC is skipped without blocking the primary.
pub fn parse_toc_entries(archive: &mut ZipArchive<File>, toc_paths: &[String]) -> Vec<TocEntry> {
    let mut entries = Vec::new();
    for toc_path in toc_paths {
        let Ok(data) = read_member(archive, toc_path) else {
            continue;
        };
        let text = decode_markup(&data);
        if toc_path.ends_with(".ncx") || looks_like_ncx(&text) {
            if let Ok(mut parsed) = parse_ncx(&text, toc_path) {
                entries.append(&mut parsed);
                continue;
            }
            // Fall through to NAV parsing on NCX failure (oracle tries both).
        }
        if let Ok(mut parsed) = parse_nav(&text, toc_path) {
            entries.append(&mut parsed);
        }
    }
    entries
}

fn looks_like_ncx(text: &str) -> bool {
    // Oracle: root local name is `ncx` or the document contains `navMap`.
    text.contains("navMap") || text.contains("<ncx")
}

/// Parse an EPUB2 NCX document (preorder, oracle `_parse_ncx`).
fn parse_ncx(text: &str, toc_path: &str) -> Result<Vec<TocEntry>> {
    struct Open {
        slot: usize,
        id: String,
        title_parts: Vec<String>,
        href: String,
        in_label: bool,
        in_text: bool,
    }
    struct Slot {
        depth: usize,
        parent: Option<usize>,
    }

    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut open: Vec<Open> = Vec::new();
    let mut slots: Vec<Slot> = Vec::new();
    // Content filled at close, indexed by preorder slot.
    let mut contents: Vec<(String, String, String)> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                let name = local_name(element.name());
                if name == "navPoint" {
                    let slot = slots.len();
                    slots.push(Slot {
                        depth: open.len(),
                        parent: open.last().map(|o: &Open| o.slot),
                    });
                    contents.push((String::new(), String::new(), String::new()));
                    open.push(Open {
                        slot,
                        id: attr_value(&element, "id"),
                        title_parts: Vec::new(),
                        href: String::new(),
                        in_label: false,
                        in_text: false,
                    });
                } else if name == "navLabel" {
                    if let Some(frame) = open.last_mut() {
                        frame.in_label = true;
                    }
                } else if name == "text" {
                    if let Some(frame) = open.last_mut() {
                        if frame.in_label {
                            frame.in_text = true;
                        }
                    }
                } else if name == "content" {
                    if let Some(frame) = open.last_mut() {
                        if frame.href.is_empty() {
                            frame.href = attr_value(&element, "src");
                        }
                    }
                }
            }
            Ok(Event::Text(element)) => {
                if let Some(frame) = open.last_mut() {
                    if frame.in_text {
                        frame
                            .title_parts
                            .push(element.xml_content(XmlVersion::Implicit1_0).into_owned());
                    }
                }
            }
            Ok(Event::End(element)) => {
                let name = local_name(element.name());
                if name == "navPoint" {
                    if let Some(frame) = open.pop() {
                        contents[frame.slot] = (
                            frame.id,
                            frame.title_parts.join("").trim().to_string(),
                            frame.href,
                        );
                    }
                } else if name == "navLabel" {
                    if let Some(frame) = open.last_mut() {
                        frame.in_label = false;
                    }
                } else if name == "text" {
                    if let Some(frame) = open.last_mut() {
                        frame.in_text = false;
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(Error::Config(format!("Invalid NCX: {e}"))),
            _ => {}
        }
        buf.clear();
    }

    let mut entries = Vec::with_capacity(slots.len());
    for (node_index, (slot, (id, title, href))) in slots.iter().zip(contents).enumerate() {
        // Oracle `_entry`: empty raw href skips resolution (all fields empty).
        let resolved = if href.is_empty() {
            None
        } else {
            Some(resolve_epub_href(toc_path, &href))
        };
        entries.push(TocEntry {
            entry_id: format!("{toc_path}:{node_index}"),
            toc_path: toc_path.to_string(),
            node_index,
            node_id: id,
            parent_index: slot.parent,
            depth: slot.depth,
            kind: "ncx".to_string(),
            title,
            raw_href: href,
            resource_href: resolved
                .as_ref()
                .map(|r| r.resource_href.clone())
                .unwrap_or_default(),
            fragment: resolved
                .as_ref()
                .map(|r| r.fragment.clone())
                .unwrap_or_default(),
            target_key: resolved
                .as_ref()
                .map(|r| r.target_key())
                .unwrap_or_default(),
            external: resolved.as_ref().is_some_and(|r| r.external),
        });
    }
    Ok(entries)
}

/// Parse an EPUB3 NAV document (oracle `_parse_nav` + scope rules).
fn parse_nav(text: &str, toc_path: &str) -> Result<Vec<TocEntry>> {
    // Minimal DOM for NAV lists: parse into a tiny tree, then walk like the oracle.
    let tree = parse_html_fragment(text);
    // Scope rule: explicit epub:type/type="toc" navs, else first nav, else whole doc.
    let navs: Vec<NodeId> = tree
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.tag.as_deref() == Some("nav"))
        .map(|(i, _)| i)
        .collect();
    let toc_navs: Vec<NodeId> = navs
        .iter()
        .copied()
        .filter(|id| {
            tree.nodes[*id]
                .attrs
                .iter()
                .any(|(k, v)| (k == "epub:type" || k == "type") && v == "toc")
        })
        .collect();
    let scopes: Vec<NodeId> = if !toc_navs.is_empty() {
        toc_navs
    } else if let Some(first) = navs.first() {
        vec![*first]
    } else {
        vec![0]
    };

    let mut entries = Vec::new();
    let mut node_index = 0usize;
    for scope in scopes {
        // Root list: direct `ol` child, else first descendant `ol`.
        let root = tree
            .children(scope)
            .into_iter()
            .find(|id| tree.nodes[*id].tag.as_deref() == Some("ol"))
            .or_else(|| {
                tree.descendants(scope)
                    .into_iter()
                    .find(|id| tree.nodes[*id].tag.as_deref() == Some("ol"))
            });
        if let Some(root) = root {
            walk_nav_list(
                &tree,
                toc_path,
                root,
                0,
                None,
                &mut entries,
                &mut node_index,
            );
        }
    }
    Ok(entries)
}

type NodeId = usize;

#[derive(Debug, Default)]
struct HtmlNode {
    tag: Option<String>,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<NodeId>,
}

#[derive(Debug, Default)]
struct HtmlTree {
    nodes: Vec<HtmlNode>,
}

impl HtmlTree {
    fn children(&self, id: NodeId) -> Vec<NodeId> {
        self.nodes[id].children.clone()
    }
    fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.nodes[id].children.iter().rev().copied().collect();
        while let Some(next) = stack.pop() {
            out.push(next);
            for child in self.nodes[next].children.iter().rev() {
                stack.push(*child);
            }
        }
        out
    }
    fn text_content(&self, id: NodeId) -> String {
        let mut out = self.nodes[id].text.clone();
        for child in &self.nodes[id].children {
            out.push_str(&self.text_content(*child));
        }
        out
    }
}

/// Lenient HTML fragment parser (good enough for NAV/heading structures).
fn parse_html_fragment(text: &str) -> HtmlTree {
    let mut tree = HtmlTree {
        nodes: vec![HtmlNode::default()],
    };
    let mut stack: Vec<NodeId> = vec![0];
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Comment / doctype / PI: skip to '>'.
            if text[i..].starts_with("<!--") {
                if let Some(end) = text[i..].find("-->") {
                    i += end + 3;
                } else {
                    break;
                }
                continue;
            }
            if let Some(end) = text[i..].find('>') {
                let tag_text = &text[i + 1..i + end];
                if let Some(close_name) = tag_text.strip_prefix('/') {
                    let close_name = close_name.trim().to_lowercase();
                    while stack.len() > 1 {
                        let top = *stack.last().unwrap();
                        stack.pop();
                        if tree.nodes[top].tag.as_deref() == Some(close_name.as_str()) {
                            break;
                        }
                    }
                } else if !tag_text.starts_with(['!', '?']) {
                    let self_closing = tag_text.trim_end().ends_with('/');
                    let mut parts = tag_text.split_whitespace();
                    let tag = parts
                        .next()
                        .unwrap_or("")
                        .trim_end_matches('/')
                        .to_lowercase();
                    if !tag.is_empty() {
                        let mut attrs = Vec::new();
                        let rest = tag_text[tag_text.find(&tag).unwrap_or(0) + tag.len()..]
                            .trim_end_matches('/')
                            .to_string();
                        attrs.extend(parse_attrs(&rest));
                        let id = tree.nodes.len();
                        tree.nodes.push(HtmlNode {
                            tag: Some(tag.clone()),
                            attrs,
                            text: String::new(),
                            children: Vec::new(),
                        });
                        tree.nodes[*stack.last().unwrap()].children.push(id);
                        let void = matches!(
                            tag.as_str(),
                            "br" | "hr" | "img" | "meta" | "link" | "input" | "col" | "base"
                        );
                        if !self_closing && !void {
                            stack.push(id);
                        }
                    }
                }
                i += end + 1;
            } else {
                break;
            }
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != b'<' {
                i += 1;
            }
            tree.nodes[*stack.last().unwrap()]
                .text
                .push_str(&text[start..i]);
        }
    }
    tree
}

fn parse_attrs(rest: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
            && bytes[i] != b'>'
        {
            i += 1;
        }
        let key = rest[start..i].to_lowercase();
        if key.is_empty() || key == "/" {
            i += 1;
            continue;
        }
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                value = rest[start..i].to_string();
                i += 1;
            } else {
                let start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                value = rest[start..i].to_string();
            }
        }
        attrs.push((key, value));
    }
    attrs
}

#[allow(clippy::too_many_arguments)]
fn walk_nav_list(
    tree: &HtmlTree,
    toc_path: &str,
    list: NodeId,
    depth: usize,
    parent_index: Option<usize>,
    entries: &mut Vec<TocEntry>,
    node_index: &mut usize,
) {
    // Direct `li` children only (oracle `recursive=False`).
    let items: Vec<NodeId> = tree
        .children(list)
        .into_iter()
        .filter(|id| tree.nodes[*id].tag.as_deref() == Some("li"))
        .collect();
    for item in items {
        // Label: direct `a` or `span` child.
        let label = tree
            .children(item)
            .into_iter()
            .find(|id| matches!(tree.nodes[*id].tag.as_deref(), Some("a") | Some("span")));
        let mut my_index = None;
        if let Some(label) = label {
            let node = &tree.nodes[label];
            let raw_href = if node.tag.as_deref() == Some("a") {
                node.attrs
                    .iter()
                    .find(|(k, _)| k == "href")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let title = tree
                .text_content(label)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let resolved = if raw_href.is_empty() {
                None
            } else {
                Some(resolve_epub_href(toc_path, &raw_href))
            };
            my_index = Some(*node_index);
            entries.push(TocEntry {
                entry_id: format!("{toc_path}:{node_index}"),
                toc_path: toc_path.to_string(),
                node_index: *node_index,
                node_id: String::new(),
                parent_index,
                depth,
                kind: "nav".to_string(),
                title,
                raw_href: raw_href.clone(),
                resource_href: resolved
                    .as_ref()
                    .map(|r| r.resource_href.clone())
                    .unwrap_or_default(),
                fragment: resolved
                    .as_ref()
                    .map(|r| r.fragment.clone())
                    .unwrap_or_default(),
                target_key: resolved
                    .as_ref()
                    .map(|r| r.target_key())
                    .unwrap_or_default(),
                external: resolved.as_ref().is_some_and(|r| r.external),
            });
            *node_index += 1;
        }
        // Recurse into direct `ol` children.
        for child in tree.children(item) {
            if tree.nodes[child].tag.as_deref() == Some("ol") {
                walk_nav_list(
                    tree,
                    toc_path,
                    child,
                    depth + 1,
                    my_index.or(parent_index),
                    entries,
                    node_index,
                );
            }
        }
    }
}
