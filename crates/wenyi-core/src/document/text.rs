//! Plain-text and Markdown reader. Oracle: `ingest/text_reader.py`.

use std::path::Path;

use crate::document::models::{Chapter, Document, Segment, KIND_HEADING, KIND_TEXT};
use crate::{Error, Result};

/// Parsed chapter heading: (title text, level 1..=6).
fn markdown_heading(line: &str) -> Option<(&str, i64)> {
    // Oracle `_MD_HEADING`: ^(#{1,6})\s+(.*\S)\s*$
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    let mut chars = rest.chars();
    if !chars.next().is_some_and(|c| c.is_whitespace()) {
        return None;
    }
    let title = rest.trim();
    if title.is_empty() {
        return None;
    }
    // Reject a 7th '#' (regex `#{1,6}` followed by `\s` excludes `####### x`).
    Some((title, hashes as i64))
}

/// Japanese chapter markers (oracle `_JA_CHAPTER`, level 1).
fn japanese_heading(line: &str) -> Option<(&str, i64)> {
    let trimmed = line.trim_start();
    const MARKERS: [&str; 10] = [
        "序章",
        "終章",
        "序幕",
        "終幕",
        "プロローグ",
        "エピローグ",
        "あとがき",
        "まえがき",
        "第", // 第 + number + 章話节回部巻 handled below
        "",
    ];
    for marker in &MARKERS[..8] {
        if trimmed.starts_with(marker) {
            return Some((trimmed, 1));
        }
    }
    if let Some(rest) = trimmed.strip_prefix('第') {
        // Consume ASCII/full-width/kanji numerals, require a volume counter.
        let mut chars = rest.char_indices();
        let mut end = 0;
        for (i, c) in chars.by_ref() {
            if c.is_ascii_digit()
                || ('０'..='９').contains(&c)
                || "一二三四五六七八九十百千".contains(c)
            {
                end = i + c.len_utf8();
            } else {
                break;
            }
        }
        if end > 0 {
            let after = &rest[end..];
            if after.starts_with(['章', '話', '节', '回', '部', '巻']) {
                return Some((trimmed, 1));
            }
        }
    }
    None
}

fn is_chapter_heading(line: &str) -> Option<(String, i64)> {
    if let Some((title, level)) = markdown_heading(line) {
        return Some((title.to_string(), level));
    }
    japanese_heading(line).map(|(title, level)| (title.to_string(), level))
}

/// Split a body block on blank lines (oracle `_split_paragraphs`).
fn split_paragraphs(block: &str) -> Vec<String> {
    // re.split(r"\n\s*\n") with strip semantics: split on blank lines,
    // drop whitespace-only pieces, strip newlines at the edges.
    let mut paragraphs = Vec::new();
    let mut current = String::new();
    for line in block.split('\n') {
        if line.trim().is_empty() {
            if !current.trim().is_empty() {
                paragraphs.push(current.trim_matches('\n').to_string());
                current.clear();
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }
    if !current.trim().is_empty() {
        paragraphs.push(current.trim_matches('\n').to_string());
    }
    paragraphs
}

/// Read UTF-8 text or Markdown, identify headings, build a `Document`.
pub fn read_text(path: &Path, source_lang: &str, target_lang: &str) -> Result<Document> {
    let content = std::fs::read_to_string(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let book_title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    // Raw chapters: (explicit title?, level, body lines).
    let mut raw: Vec<(Option<String>, i64, Vec<String>)> = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_level: i64 = 1;
    let mut body: Vec<String> = Vec::new();
    for line in content.split('\n') {
        // `splitlines` compatibility: strip a trailing '\r' per line.
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some((title, level)) = is_chapter_heading(line) {
            if current_title.is_some() || !body.is_empty() {
                raw.push((
                    current_title.take(),
                    current_level,
                    std::mem::take(&mut body),
                ));
            }
            current_title = Some(title);
            current_level = level;
        } else {
            body.push(line.to_string());
        }
    }
    if current_title.is_some() || !body.is_empty() {
        raw.push((current_title, current_level, body));
    }

    let mut chapters = Vec::with_capacity(raw.len());
    for (index, (explicit_title, level, body_lines)) in raw.into_iter().enumerate() {
        let title = explicit_title.clone().unwrap_or_else(|| book_title.clone());
        let mut segments = Vec::new();
        if let Some(heading) = explicit_title {
            segments.push(Segment {
                index: 0,
                source: heading,
                kind: KIND_HEADING.to_string(),
                target: None,
                target_before_polish: None,
                anchor: None,
                resource_href: None,
                cont: false,
                meta: serde_json::Map::new(),
            });
        }
        let joined = body_lines.join("\n");
        for paragraph in split_paragraphs(&joined) {
            let mut segment = Segment::text(segments.len(), paragraph);
            segment.kind = KIND_TEXT.to_string();
            segments.push(segment);
        }
        let mut meta = serde_json::Map::new();
        meta.insert("heading_level".to_string(), serde_json::Value::from(level));
        chapters.push(Chapter {
            index,
            title,
            segments,
            href: None,
            template: None,
            meta,
        });
    }

    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    Ok(Document {
        title: book_title,
        source_lang: source_lang.to_string(),
        target_lang: target_lang.to_string(),
        fmt: "text".to_string(),
        source_path: absolute.to_string_lossy().into_owned(),
        chapters,
        meta: serde_json::Map::new(),
    })
}
