//! Minimal DOCX writer. Oracle: `assemble/docx_*.py`.
//!
//! Builds a Word-readable `.docx` (ZIP + `word/document.xml`) directly with
//! the vendored `zip` crate — no `python-docx` equivalent exists offline.
//! Covers the oracle's structural surface: headings (outline levels),
//! normal paragraphs, bilingual pairs (dim 9pt `#888` secondary), simple
//! `Table Grid` tables, target-font runs, align/shade, and proportional
//! style slices. List numbering uses literal text (no `numbering.xml`
//! restart overrides); annotation/docx-style realignment hooks land with
//! their port slices.

use std::io::Write;
use std::path::Path;

use zip::write::SimpleFileOptions;

use crate::document::assemble::{bilingual_source, escape_html, ordered_pair, seg_text};
use crate::document::models::{Chapter, Document, KIND_HEADING};
use crate::{Error, Result};

/// One run of text with an optional style payload.
pub struct StyledRun {
    /// Run text.
    pub text: String,
    /// Style map (`bold`/`italic`/`underline`/`size_pt`/`color`).
    pub style: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Split text into fragment/style pairs (oracle `_style_slices`).
pub fn style_slices(
    text: &str,
    style: Option<&serde_json::Map<String, serde_json::Value>>,
    placements: Option<&[serde_json::Value]>,
) -> Vec<StyledRun> {
    if text.is_empty() {
        return Vec::new();
    }
    let Some(placements) = placements else {
        return vec![StyledRun {
            text: text.to_string(),
            style: style.cloned(),
        }];
    };
    if placements.is_empty() {
        return vec![StyledRun {
            text: text.to_string(),
            style: style.cloned(),
        }];
    }
    let len = text.chars().count();
    let mut bounds = vec![0, len];
    let mut usable = Vec::new();
    for row in placements {
        let Some(obj) = row.as_object() else { continue };
        let (Some(start), Some(end)) = (
            obj.get("target_start").and_then(|v| v.as_i64()),
            obj.get("target_end").and_then(|v| v.as_i64()),
        ) else {
            continue;
        };
        let start = start.max(0).min(len as i64) as usize;
        let end = (end.max(start as i64) as usize).min(len);
        if start >= end {
            continue;
        }
        bounds.push(start);
        bounds.push(end);
        usable.push((
            start,
            end,
            obj.get("style").and_then(|v| v.as_object()).cloned(),
        ));
    }
    if usable.is_empty() {
        return vec![StyledRun {
            text: text.to_string(),
            style: style.cloned(),
        }];
    }
    bounds.sort();
    bounds.dedup();
    let chars: Vec<char> = text.chars().collect();
    bounds
        .windows(2)
        .map(|window| {
            let (start, end) = (window[0], window[1]);
            let fragment: String = chars[start..end].iter().collect();
            let run_style = usable
                .iter()
                .find(|(placement_start, placement_end, _)| {
                    *placement_start <= start && start < *placement_end
                })
                .and_then(|(_, _, style)| style.clone())
                .or_else(|| style.cloned());
            StyledRun {
                text: fragment,
                style: run_style,
            }
        })
        .collect()
}

/// One table grid: (row, col) -> (target, source, meta).
type TableCells = std::collections::BTreeMap<
    (i64, i64),
    (String, String, serde_json::Map<String, serde_json::Value>),
>;

/// Target font for translations only (oracle `_font_for_text`).
pub fn font_for_text(output_font: Option<&str>, source: &str, output_text: &str) -> Option<String> {
    let output_font = output_font?;
    if output_text.trim().is_empty() || output_text == source {
        return None;
    }
    Some(output_font.to_string())
}

/// Render one run with style + optional font override + dim.
fn render_run(
    out: &mut String,
    text: &str,
    style: Option<&serde_json::Map<String, serde_json::Value>>,
    output_font: Option<&str>,
    dim: bool,
) {
    let mut props = String::new();
    if let Some(font) = output_font {
        let escaped = escape_html(font);
        props += &format!(
            "<w:rFonts w:ascii=\"{escaped}\" w:hAnsi=\"{escaped}\" w:eastAsia=\"{escaped}\" w:cs=\"{escaped}\"/>"
        );
    }
    if let Some(style) = style {
        if style.get("bold") == Some(&serde_json::Value::Bool(true)) {
            props += "<w:b/>";
        }
        if style.get("italic") == Some(&serde_json::Value::Bool(true)) {
            props += "<w:i/>";
        }
        if style
            .get("underline")
            .is_some_and(|v| v != &serde_json::Value::Bool(false) && !v.is_null())
        {
            props += "<w:u w:val=\"single\"/>";
        }
        if let Some(size) = style.get("size_pt").and_then(|v| v.as_f64()) {
            if size > 0.0 {
                props += &format!("<w:sz w:val=\"{}\"/>", (size * 2.0).round() as i64);
            }
        }
        if let Some(color) = style.get("color").and_then(|v| v.as_str()) {
            if color.len() >= 6 {
                props += &format!("<w:color w:val=\"{}\"/>", &color[color.len() - 6..]);
            }
        }
    }
    if dim {
        props += "<w:sz w:val=\"18\"/><w:color w:val=\"888888\"/>";
    }
    out.push_str("<w:r>");
    if !props.is_empty() {
        out.push_str("<w:rPr>");
        out.push_str(&props);
        out.push_str("</w:rPr>");
    }
    // Preserve leading/trailing spaces like python-docx (`xml:space`).
    out.push_str("<w:t xml:space=\"preserve\">");
    out.push_str(&escape_html(text));
    out.push_str("</w:t></w:r>");
}

/// Fill one paragraph with styled runs (oracle `_fill_paragraph` core).
#[allow(clippy::too_many_arguments)] // Mirrors the oracle fill signature.
fn fill_paragraph(
    out: &mut String,
    text: &str,
    style: Option<&serde_json::Map<String, serde_json::Value>>,
    placements: Option<&[serde_json::Value]>,
    align: Option<&str>,
    shade: Option<&str>,
    output_font: Option<&str>,
    dim: bool,
    style_name: Option<&str>,
    outline_level: Option<i64>,
) {
    out.push_str("<w:p>");
    let mut ppr = String::new();
    if let Some(name) = style_name {
        ppr += &format!("<w:pStyle w:val=\"{}\"/>", escape_html(name));
    }
    if let Some(level) = outline_level {
        ppr += &format!("<w:outlineLvl w:val=\"{}\"/>", level.clamp(1, 9) - 1);
    }
    if let Some(align) = align {
        let value = match align {
            "left" => "left",
            "center" => "center",
            "right" => "right",
            "justify" | "both" => "both",
            "distribute" => "distribute",
            _ => "left",
        };
        ppr += &format!("<w:jc w:val=\"{value}\"/>");
    }
    if let Some(shade) = shade {
        if shade.len() >= 6 {
            ppr += &format!(
                "<w:shd w:val=\"clear\" w:fill=\"{}\"/>",
                &shade[shade.len() - 6..]
            );
        }
    }
    if !ppr.is_empty() {
        out.push_str("<w:pPr>");
        out.push_str(&ppr);
        out.push_str("</w:pPr>");
    }
    for run in style_slices(text, style, placements) {
        render_run(out, &run.text, run.style.as_ref(), output_font, dim);
    }
    out.push_str("</w:p>");
}

/// Add a heading (oracle `_add_heading`).
fn add_heading(out: &mut String, text: &str, level: i64, output_font: Option<&str>) {
    let level = level.clamp(1, 9);
    fill_paragraph(
        out,
        text,
        None,
        None,
        None,
        None,
        output_font,
        false,
        Some(&format!("Heading {level}")),
        Some(level),
    );
}

/// Style payload from segment meta (uniform-style subset of oracle
/// `_segment_style_payload`; proportional placement mapping is skipped —
/// placements apply when present and valid).
#[allow(clippy::type_complexity)]
fn style_payload(
    meta: &serde_json::Map<String, serde_json::Value>,
) -> (
    Option<serde_json::Map<String, serde_json::Value>>,
    Option<Vec<serde_json::Value>>,
    Option<String>,
    Option<String>,
) {
    let style = meta.get("docx_style").and_then(|v| v.as_object()).cloned();
    let placements = meta
        .get("docx_placements")
        .and_then(|v| v.as_array())
        .cloned();
    let align = meta
        .get("align")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let shade = meta
        .get("shade")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    (style, placements, align, shade)
}

/// Emit one chapter's blocks (oracle `_emit_chapter_blocks`).
fn emit_chapter(
    out: &mut String,
    chapter: &Chapter,
    bilingual: bool,
    order: &str,
    output_font: Option<&str>,
) {
    let mut index = 0usize;
    let segments = &chapter.segments;
    while index < segments.len() {
        let segment = &segments[index];
        // Table group: contiguous `table_id` cells flushed as one grid.
        if let Some(table_id) = segment.meta.get("table_id").and_then(|v| v.as_i64()) {
            let mut cells: TableCells = TableCells::new();
            let mut rows = segment
                .meta
                .get("rows")
                .and_then(|v| v.as_i64())
                .unwrap_or(1);
            let mut cols = segment
                .meta
                .get("cols")
                .and_then(|v| v.as_i64())
                .unwrap_or(1);
            while index < segments.len() {
                let current = &segments[index];
                if current.meta.get("table_id").and_then(|v| v.as_i64()) != Some(table_id) {
                    break;
                }
                let row = current
                    .meta
                    .get("row")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let col = current
                    .meta
                    .get("col")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                rows = rows.max(
                    current
                        .meta
                        .get("rows")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(rows),
                );
                cols = cols.max(
                    current
                        .meta
                        .get("cols")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(cols),
                );
                cells.insert(
                    (row, col),
                    (
                        seg_text(current).to_string(),
                        current.source.clone(),
                        current.meta.clone(),
                    ),
                );
                index += 1;
            }
            flush_table(out, &cells, rows, cols, bilingual, order, output_font);
            continue;
        }
        if segment.source.trim().is_empty()
            && segment
                .target
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
        {
            index += 1;
            continue;
        }
        // Merge continuations.
        let mut target_parts = vec![seg_text(segment).to_string()];
        let mut source_parts = vec![segment.source.clone()];
        let kind = segment.kind.clone();
        let meta = segment.meta.clone();
        index += 1;
        while index < segments.len() {
            let next = &segments[index];
            if !next.cont || next.meta.get("table_id").is_some() {
                break;
            }
            target_parts.push(seg_text(next).to_string());
            source_parts.push(next.source.clone());
            index += 1;
        }
        let target = target_parts.concat();
        let source = source_parts.concat();
        let (style, placements, align, shade) = style_payload(&meta);
        let text_font = font_for_text(output_font, &source, &target);
        if kind == KIND_HEADING {
            let level = meta
                .get("heading_level")
                .and_then(|v| v.as_i64())
                .unwrap_or(1);
            add_heading(out, &target, level, text_font.as_deref());
        } else if bilingual {
            let src = bilingual_source(&source, &target);
            if src.is_empty() {
                fill_paragraph(
                    out,
                    &target,
                    style.as_ref(),
                    placements.as_deref(),
                    align.as_deref(),
                    shade.as_deref(),
                    text_font.as_deref(),
                    false,
                    None,
                    None,
                );
            } else {
                let (first, second) = ordered_pair(&src, &target, order);
                let first_is_source = first == src;
                for (text, is_source) in [(first, first_is_source), (second, !first_is_source)] {
                    fill_paragraph(
                        out,
                        text,
                        if is_source { None } else { style.as_ref() },
                        if is_source {
                            None
                        } else {
                            placements.as_deref()
                        },
                        align.as_deref(),
                        shade.as_deref(),
                        if is_source {
                            None
                        } else {
                            text_font.as_deref()
                        },
                        is_source && order != "source_first",
                        None,
                        None,
                    );
                }
            }
        } else {
            fill_paragraph(
                out,
                &target,
                style.as_ref(),
                placements.as_deref(),
                align.as_deref(),
                shade.as_deref(),
                text_font.as_deref(),
                false,
                None,
                None,
            );
        }
    }
}

/// Flush one table grid (oracle `_flush_table`).
fn flush_table(
    out: &mut String,
    cells: &TableCells,
    rows: i64,
    cols: i64,
    bilingual: bool,
    order: &str,
    output_font: Option<&str>,
) {
    out.push_str("<w:tbl><w:tblPr><w:tblStyle w:val=\"TableGrid\"/></w:tblPr>");
    for row in 0..rows {
        out.push_str("<w:tr>");
        for col in 0..cols {
            let (target, source, meta) = cells.get(&(row, col)).cloned().unwrap_or_default();
            let (style, placements, align, shade) = style_payload(&meta);
            let text_font = font_for_text(output_font, &source, &target);
            out.push_str("<w:tc><w:p>");
            if bilingual {
                let src = bilingual_source(&source, &target);
                if src.is_empty() {
                    out.push_str("</w:p>");
                    let mut cell = String::new();
                    fill_paragraph(
                        &mut cell,
                        &target,
                        style.as_ref(),
                        placements.as_deref(),
                        align.as_deref(),
                        shade.as_deref(),
                        text_font.as_deref(),
                        false,
                        None,
                        None,
                    );
                    // Replace the empty paragraph with the filled one.
                    out.truncate(out.len() - 6);
                    out.push_str(&cell);
                } else if order == "source_first" {
                    out.push_str("</w:p>");
                    let mut cell = String::new();
                    fill_paragraph(
                        &mut cell,
                        &src,
                        None,
                        None,
                        align.as_deref(),
                        shade.as_deref(),
                        None,
                        false,
                        None,
                        None,
                    );
                    // Second line in the same paragraph: strip the closing
                    // tag, add a break, then the translation runs.
                    cell.truncate(cell.len() - 6);
                    cell.push_str("<w:r><w:br/></w:r>");
                    for run in style_slices(&target, style.as_ref(), placements.as_deref()) {
                        render_run(
                            &mut cell,
                            &run.text,
                            run.style.as_ref(),
                            text_font.as_deref(),
                            false,
                        );
                    }
                    cell.push_str("</w:p>");
                    out.truncate(out.len() - 6);
                    out.push_str(&cell);
                } else {
                    out.push_str("</w:p>");
                    let mut cell = String::new();
                    fill_paragraph(
                        &mut cell,
                        &target,
                        style.as_ref(),
                        placements.as_deref(),
                        align.as_deref(),
                        shade.as_deref(),
                        text_font.as_deref(),
                        false,
                        None,
                        None,
                    );
                    cell.truncate(cell.len() - 6);
                    cell.push_str("<w:r><w:br/></w:r>");
                    render_run(&mut cell, &src, None, None, true);
                    cell.push_str("</w:p>");
                    out.truncate(out.len() - 6);
                    out.push_str(&cell);
                }
            } else {
                out.push_str("</w:p>");
                let mut cell = String::new();
                fill_paragraph(
                    &mut cell,
                    &target,
                    style.as_ref(),
                    placements.as_deref(),
                    align.as_deref(),
                    shade.as_deref(),
                    text_font.as_deref(),
                    false,
                    None,
                    None,
                );
                out.truncate(out.len() - 6);
                out.push_str(&cell);
            }
            out.push_str("</w:tc>");
        }
        out.push_str("</w:tr>");
    }
    out.push_str("</w:tbl>");
}

/// Assemble a DOCX export (oracle `docx_writer._assemble_docx`).
pub fn assemble_docx(
    document: &Document,
    out_path: &Path,
    bilingual: bool,
    order: &str,
    output_font: Option<&str>,
) -> Result<()> {
    let mut body = String::new();
    for chapter in &document.chapters {
        // Explicit chapter title without an h1 segment becomes a heading.
        let has_h1 = chapter.segments.iter().any(|segment| {
            segment.kind == KIND_HEADING
                && segment
                    .meta
                    .get("heading_level")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(1)
                    == 1
        });
        if !chapter.title.trim().is_empty()
            && !has_h1
            && chapter
                .meta
                .get("explicit_title")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        {
            add_heading(&mut body, chapter.title.trim(), 1, output_font);
        }
        emit_chapter(&mut body, chapter, bilingual, order, output_font);
    }
    let document_xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{body}</w:body></w:document>"
    );
    let content_types = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
         <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
         <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
         </Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
         </Relationships>";
    // Atomic publish: same-directory temp + rename (stronger than the
    // oracle's direct `doc.save`).
    let tmp_path = out_path.with_extension(
        out_path
            .extension()
            .map(|ext| format!("{}.tmp", ext.to_string_lossy()))
            .unwrap_or_else(|| "tmp".to_string()),
    );
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                path: parent.to_path_buf(),
                message: e.to_string(),
            })?;
        }
    }
    {
        let file = std::fs::File::create(&tmp_path).map_err(|e| Error::Io {
            path: tmp_path.clone(),
            message: e.to_string(),
        })?;
        let mut writer = zip::ZipWriter::new(file);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", rels),
            ("word/document.xml", document_xml.as_str()),
        ] {
            writer.start_file(name, options).map_err(|e| Error::Io {
                path: tmp_path.clone(),
                message: e.to_string(),
            })?;
            writer
                .write_all(content.as_bytes())
                .map_err(|e| Error::Io {
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
