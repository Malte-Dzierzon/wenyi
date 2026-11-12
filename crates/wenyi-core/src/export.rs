//! Export orchestration: dispatch, reports, atomic publish.
//!
//! Oracle: `assemble/writer.py` + `report.py` + `pipeline/finalization.py`
//! (report half). Reads a consistent store snapshot (chapters + formal
//! targets), builds a `Document`, dispatches per format, and publishes
//! atomically. Reports summarize progress, empty targets, conflicts, and
//! the latest finished review.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::json;

use crate::document::assemble::AssembleOptions;
use crate::document::models::{Chapter, Document, Segment};
use crate::model::{BookId, ProjectId};
use crate::{store, Error, Result};

/// Supported export formats (oracle `_OUT_EXT` keys minus PDF backends).
pub const EXPORT_FORMATS: [&str; 5] = ["epub", "txt", "html", "markdown", "docx"];

/// Extension per format (oracle `_OUT_EXT`).
pub fn export_extension(format: &str) -> Option<&'static str> {
    match format {
        "epub" => Some(".epub"),
        "txt" => Some(".txt"),
        "html" => Some(".html"),
        "markdown" => Some(".md"),
        "docx" => Some(".docx"),
        _ => None,
    }
}

/// Sanitize a filename cross-platform (oracle `_sanitize_filename`).
pub fn sanitize_filename(name: &str, fallback: &str) -> String {
    let mut cleaned: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\r' | '\n' | '\t' => ' ',
            _ => c,
        })
        .collect();
    cleaned = cleaned.trim().trim_matches('.').to_string();
    let mut collapsed = String::with_capacity(cleaned.len());
    let mut gap = false;
    for word in cleaned.split_whitespace() {
        if gap {
            collapsed.push(' ');
        }
        collapsed.push_str(word);
        gap = true;
    }
    let truncated: String = collapsed.chars().take(120).collect();
    if truncated.is_empty() {
        fallback.to_string()
    } else {
        truncated
    }
}

/// Derive the bilingual path from an explicit path (oracle
/// `bilingual_out_path`).
pub fn bilingual_out_path(out_path: &Path) -> PathBuf {
    match out_path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => {
            let stem = out_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("translated");
            out_path.with_file_name(format!("{stem}-bi.{ext}"))
        }
        None => {
            let name = out_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("translated");
            out_path.with_file_name(format!("{name}-bi"))
        }
    }
}

/// Default export path under the input's `output/` folder (oracle
/// `_default_out`).
pub fn default_out_path(
    source_path: &Path,
    format: &str,
    title: Option<&str>,
    bilingual: bool,
    target_lang: &str,
) -> Result<PathBuf> {
    let ext = export_extension(format)
        .ok_or_else(|| Error::Config(format!("Unsupported export format: {format}")))?;
    let absolute = if source_path.is_absolute() {
        source_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| Error::Io {
                path: source_path.to_path_buf(),
                message: e.to_string(),
            })?
            .join(source_path)
    };
    let output_dir = absolute.parent().unwrap_or(Path::new(".")).join("output");
    std::fs::create_dir_all(&output_dir).map_err(|e| Error::Io {
        path: output_dir.clone(),
        message: e.to_string(),
    })?;
    if let Some(title) = title {
        if !title.trim().is_empty() {
            return Ok(output_dir.join(sanitize_filename(title, "translated") + ext));
        }
    }
    let stem = absolute
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("translated");
    let lang = crate::i18n::require_language(target_lang, false)?;
    let suffix = if bilingual { "-bi" } else { "" };
    Ok(output_dir.join(format!("{stem}.{lang}{suffix}{ext}")))
}

/// Build a `Document` snapshot from the store (formal targets only).
pub fn snapshot_document(
    conn: &Connection,
    book_id: &BookId,
    source_lang: &str,
    target_lang: &str,
    fmt: &str,
    title: &str,
) -> Result<Document> {
    let mut chapters = Vec::new();
    for chapter_row in store::list_chapters(conn, book_id, 0, i64::MAX)? {
        let mut segments = Vec::new();
        for (index, row) in store::list_segments(conn, &chapter_row.id, 0, i64::MAX)?
            .into_iter()
            .enumerate()
        {
            segments.push(Segment {
                index,
                source: row.source,
                kind: row.kind,
                target: row.target,
                target_before_polish: row.target_before_polish,
                anchor: if row.anchor.is_empty() {
                    None
                } else {
                    Some(row.anchor)
                },
                resource_href: None,
                cont: false,
                meta: serde_json::Map::new(),
            });
        }
        // Prefer the translated chapter title (oracle `_ch_title`).
        let title = chapter_row
            .title_translated
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or(chapter_row.title.clone());
        chapters.push(Chapter {
            index: chapter_row.ordinal as usize,
            title,
            segments,
            href: None,
            template: None,
            meta: serde_json::Map::new(),
        });
    }
    Ok(Document {
        title: title.to_string(),
        source_lang: source_lang.to_string(),
        target_lang: target_lang.to_string(),
        fmt: fmt.to_string(),
        source_path: String::new(),
        chapters,
        meta: serde_json::Map::new(),
    })
}

/// Export one book snapshot to a file (oracle `assemble`).
#[allow(clippy::too_many_arguments)]
pub fn export_book(
    conn: &Connection,
    book_id: &BookId,
    source_path: &Path,
    out_path: Option<&Path>,
    format: &str,
    title: Option<&str>,
    source_lang: &str,
    target_lang: &str,
    fmt: &str,
    book_title: &str,
    options: &AssembleOptions,
    output_font: Option<&str>,
    events: &mut Vec<serde_json::Value>,
) -> Result<PathBuf> {
    let document = snapshot_document(conn, book_id, source_lang, target_lang, fmt, book_title)?;
    let out_path = match out_path {
        Some(path) => path.to_path_buf(),
        None => default_out_path(source_path, format, title, options.bilingual, target_lang)?,
    };
    match format {
        "txt" => crate::document::assemble::assemble_text(&document, &out_path, options, false)?,
        "markdown" => {
            crate::document::assemble::assemble_text(&document, &out_path, options, true)?;
        }
        "html" => crate::document::assemble::assemble_html(&document, &out_path, options)?,
        "docx" => crate::document::docx::assemble_docx(
            &document,
            &out_path,
            options.bilingual,
            &options.order,
            output_font,
        )?,
        "epub" => {
            if fmt == "epub" {
                crate::document::assemble::assemble_epub(
                    &document,
                    source_path,
                    &out_path,
                    options,
                )?;
            } else {
                return Err(Error::Config(
                    "EPUB export from non-EPUB sources is not implemented yet.".to_string(),
                ));
            }
        }
        _ => {
            return Err(Error::Config(format!(
                "Unsupported export format: {format}"
            )));
        }
    }
    events.push(json!({
        "event": "assembled",
        "outputs": [out_path.to_string_lossy()],
        "out_format": format,
    }));
    Ok(out_path)
}

/// Full report: progress, empty targets, conflicts, latest review (oracle
/// `build_report`).
pub fn build_report(
    conn: &Connection,
    project_id: &ProjectId,
    book_id: &BookId,
    source_lang: &str,
    target_lang: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let chapters = store::list_chapters(conn, book_id, 0, i64::MAX)?;
    let chapters_total = chapters.len() as i64;
    let chapters_done = chapters
        .iter()
        .filter(|chapter| chapter.status == "done")
        .count() as i64;
    let mut empty_targets = Vec::new();
    for chapter in &chapters {
        if chapter.status != "done" {
            continue;
        }
        for (ordinal, _kind, source, target) in store::iter_segments(conn, &chapter.id)? {
            if source.trim().is_empty() {
                continue;
            }
            if target.as_deref().unwrap_or_default().trim().is_empty() {
                empty_targets.push(json!({
                    "chapter": chapter.ordinal,
                    "index": ordinal,
                    "source": source.chars().take(60).collect::<String>(),
                }));
            }
        }
    }
    let conflicts = crate::glossary::open_conflicts(conn, project_id, source_lang, target_lang)?;
    let (terms, _) = crate::glossary::stats(conn, project_id, source_lang, target_lang)?;
    let mut report = serde_json::Map::new();
    report.insert(
        "summary".to_string(),
        json!({
            "chapters_total": chapters_total,
            "chapters_done": chapters_done,
            "terms": terms,
            "open_conflicts": conflicts.len() as i64,
            "empty_targets": empty_targets.len() as i64,
        }),
    );
    report.insert(
        "open_conflicts".to_string(),
        json!(conflicts
            .into_iter()
            .map(|row| serde_json::Value::Object(row.into_iter().collect()))
            .collect::<Vec<_>>()),
    );
    report.insert(
        "empty_targets".to_string(),
        serde_json::Value::Array(empty_targets),
    );
    if let Some(review) = store::latest_finished_review(conn, book_id)? {
        let summary: serde_json::Value =
            serde_json::from_str(&review.summary_json).unwrap_or_default();
        let autofix = summary.get("autofix").cloned().unwrap_or(json!({}));
        let applied = autofix
            .get("applied_segment_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        report.insert(
            "review".to_string(),
            json!({
                "review_id": review.id,
                "status": review.status,
                "termination": review.termination,
                "issue_count": summary.get("issue_count").and_then(|v| v.as_i64()).unwrap_or(0),
                "change_count": summary.get("change_count").and_then(|v| v.as_i64()).unwrap_or(0),
                "read_only": applied == 0,
                "autofix_status": autofix.get("status"),
                "autofix_applied_segment_count": applied,
                "autofix_failed_issue_count": autofix.get("failed_issue_count").and_then(|v| v.as_i64()).unwrap_or(0),
            }),
        );
    }
    Ok(report)
}

/// Summary-only counts without chapter bodies (oracle `build_summary`).
pub fn build_summary(
    conn: &Connection,
    project_id: &ProjectId,
    book_id: &BookId,
    source_lang: &str,
    target_lang: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let chapters = store::list_chapters(conn, book_id, 0, i64::MAX)?;
    let chapters_total = chapters.len() as i64;
    let mut chapters_done = 0i64;
    let mut empty_targets = 0i64;
    for chapter in &chapters {
        if chapter.status != "done" {
            continue;
        }
        chapters_done += 1;
        for (_ordinal, _kind, source, target) in store::iter_segments(conn, &chapter.id)? {
            if source.trim().is_empty() {
                continue;
            }
            if target.as_deref().unwrap_or_default().trim().is_empty() {
                empty_targets += 1;
            }
        }
    }
    let (terms, open_conflicts) =
        crate::glossary::stats(conn, project_id, source_lang, target_lang)?;
    let mut report = serde_json::Map::new();
    report.insert(
        "summary".to_string(),
        json!({
            "chapters_total": chapters_total,
            "chapters_done": chapters_done,
            "terms": terms,
            "open_conflicts": open_conflicts,
            "empty_targets": empty_targets,
        }),
    );
    Ok(report)
}

/// Persist the report under the book's artifacts (oracle `save_report`).
pub fn save_report(
    conn: &Connection,
    project_id: &ProjectId,
    book_id: &BookId,
    report: &serde_json::Map<String, serde_json::Value>,
    events: &mut Vec<serde_json::Value>,
) -> Result<()> {
    store::save_artifact(
        conn,
        project_id,
        &format!("report/{}/report.json", book_id.as_str()),
        &serde_json::Value::Object(report.clone()).to_string(),
    )?;
    events.push(json!({"event": "report_saved", "artifact": "report.json"}));
    Ok(())
}

/// Silence unused-import lints for report-only builds.
#[allow(dead_code)]
fn _export_hint(_: &BTreeMap<String, String>) {}
