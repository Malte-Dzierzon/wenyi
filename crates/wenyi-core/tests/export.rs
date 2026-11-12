//! Export parity: writers, bilingual modes, reports, atomic publish.

use std::path::PathBuf;
use wenyi_core::document::assemble::AssembleOptions;
use wenyi_core::export::{
    bilingual_out_path, build_report, build_summary, default_out_path, export_book,
    sanitize_filename, save_report, snapshot_document,
};
use wenyi_core::model::{BookId, ChapterId, ProjectId, SegmentId};
use wenyi_core::store;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wenyi-core-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn seed(name: &str) -> (PathBuf, ProjectId, BookId) {
    let dir = temp_dir(name);
    let db = store::db_path(&dir);
    let conn = store::open(&db).unwrap();
    let project = ProjectId::new("p").unwrap();
    let book = BookId::new("b").unwrap();
    store::create_project(&conn, &project, "t").unwrap();
    store::create_book(&conn, &book, &project, 0, "B").unwrap();
    let chapter = ChapterId::new("c0").unwrap();
    store::create_chapter(&conn, &chapter, &book, 0, "Chapter One").unwrap();
    store::set_title_translated(&conn, &chapter, Some("第一章")).unwrap();
    let pairs: Vec<(SegmentId, String)> = ["The old Wolf prowls.", "Moon rises."]
        .into_iter()
        .enumerate()
        .map(|(i, source)| (SegmentId::new(format!("s{i}")).unwrap(), source.to_string()))
        .collect();
    store::insert_segments(&conn, &chapter, &pairs).unwrap();
    store::set_target(
        &conn,
        &SegmentId::new("s0").unwrap(),
        Some("老狼潜行。"),
        "translate",
    )
    .unwrap();
    store::set_chapter_status(&conn, &chapter, "done").unwrap();
    drop(conn);
    (dir, project, book)
}

#[test]
fn paths_match_oracle() {
    assert_eq!(sanitize_filename("a/b:c*d", "translated"), "a b c d");
    assert_eq!(sanitize_filename("", "translated"), "translated");
    let bilingual = bilingual_out_path(&PathBuf::from("out/novel.zh.epub"));
    assert_eq!(bilingual, PathBuf::from("out/novel.zh-bi.epub"));
    let dir = temp_dir("export-paths");
    let source = dir.join("novel.txt");
    std::fs::write(&source, "x").unwrap();
    let out = default_out_path(&source, "txt", None, false, "zh").unwrap();
    assert_eq!(out, dir.join("output").join("novel.zh.txt"));
    let out_bi = default_out_path(&source, "epub", None, true, "zh").unwrap();
    assert_eq!(out_bi, dir.join("output").join("novel.zh-bi.epub"));
}

#[test]
fn text_html_and_docx_round_trip_store_snapshot() {
    let (dir, project, book) = seed("writers");
    let db = store::db_path(&dir);
    let conn = store::open(&db).unwrap();
    let document = snapshot_document(&conn, &book, "en", "zh", "text", "B").unwrap();
    assert_eq!(document.chapters[0].title, "第一章");
    let mono = AssembleOptions::monolingual();
    let text_path = dir.join("out.txt");
    wenyi_core::document::assemble::assemble_text(&document, &text_path, &mono, false).unwrap();
    let text = std::fs::read_to_string(&text_path).unwrap();
    assert!(text.contains("老狼潜行。"), "{text}");
    // Untranslated segments fall back to source (no content loss).
    assert!(text.contains("Moon rises."), "{text}");
    // Bilingual target-first: translation then muted source.
    let bilingual = AssembleOptions {
        bilingual: true,
        order: "target_first".to_string(),
        preserve_source_style: false,
        preserve_source_ruby: None,
    };
    let html_path = dir.join("out.html");
    wenyi_core::document::assemble::assemble_html(&document, &html_path, &bilingual).unwrap();
    let html = std::fs::read_to_string(&html_path).unwrap();
    assert!(html.contains("lang=\"zh-Hans\""), "{html}");
    assert!(html.contains("tn-bilingual-style"), "{html}");
    let target_pos = html.find("老狼潜行").unwrap();
    let source_pos = html.find("The old Wolf").unwrap();
    assert!(target_pos < source_pos, "target-first order");
    // DOCX: valid zip with the translation inside document.xml.
    let docx_path = dir.join("out.docx");
    wenyi_core::document::docx::assemble_docx(
        &document,
        &docx_path,
        false,
        "target_first",
        Some("宋体"),
    )
    .unwrap();
    let file = std::fs::File::open(&docx_path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("word/document.xml").unwrap(), &mut xml)
        .unwrap();
    assert!(xml.contains("老狼潜行"), "{xml}");
    assert!(xml.contains("宋体"), "{xml}");
    // No temp files leak beside the outputs.
    assert!(!dir.join("out.txt.tmp").exists());
    let _ = project;
}

#[test]
fn srt_writer_matches_oracle_blocks() {
    use std::collections::BTreeMap;
    let dir = temp_dir("export-srt");
    let cues = wenyi_core::document::srt::parse_srt_text(
        "1\n00:00:01,000 --> 00:00:04,000\nHi\n\n2\n00:00:05,000 --> 00:00:06,000\nBye\n",
    )
    .unwrap();
    let mut translations = BTreeMap::new();
    translations.insert("1".to_string(), "你好".to_string());
    let mono = dir.join("a.zh.srt");
    let bilingual = dir.join("a.zh-bi.srt");
    let written = wenyi_core::document::srt::write_srt_outputs(
        &cues,
        &translations,
        Some(&mono),
        Some(&bilingual),
    )
    .unwrap();
    assert_eq!(written.len(), 2);
    let mono_text = std::fs::read_to_string(&mono).unwrap();
    assert!(
        mono_text.contains("1\n00:00:01,000 --> 00:00:04,000\n你好\n"),
        "{mono_text}"
    );
    assert!(
        mono_text.contains("2\n00:00:05,000 --> 00:00:06,000\nBye\n"),
        "{mono_text}"
    );
    let bi_text = std::fs::read_to_string(&bilingual).unwrap();
    assert!(bi_text.contains("你好\nHi\n"), "{bi_text}");
}

#[test]
fn export_book_dispatches_and_reports_summarize() {
    let (dir, project, book) = seed("dispatch");
    let db = store::db_path(&dir);
    let conn = store::open(&db).unwrap();
    let source = dir.join("novel.txt");
    std::fs::write(&source, "x").unwrap();
    let mut events = Vec::new();
    let out = export_book(
        &conn,
        &book,
        &source,
        None,
        "txt",
        None,
        "en",
        "zh",
        "text",
        "B",
        &AssembleOptions::monolingual(),
        None,
        &mut events,
    )
    .unwrap();
    assert!(out.ends_with("novel.zh.txt"), "{out:?}");
    assert!(events.iter().any(|e| e["event"] == "assembled"));
    let summary = build_summary(&conn, &project, &book, "en", "zh").unwrap();
    assert_eq!(summary["summary"]["chapters_total"], 1);
    assert_eq!(summary["summary"]["chapters_done"], 1);
    assert_eq!(summary["summary"]["empty_targets"], 1);
    let report = build_report(&conn, &project, &book, "en", "zh").unwrap();
    assert_eq!(report["empty_targets"].as_array().unwrap().len(), 1);
    assert!(report.get("review").is_none());
    save_report(&conn, &project, &book, &report, &mut events).unwrap();
    assert!(events.iter().any(|e| e["event"] == "report_saved"));
    let stored = store::load_artifact(&conn, &project, "report/b/report.json")
        .unwrap()
        .unwrap();
    assert!(stored.contains("empty_targets"));
}

/// Build the oracle parity document: heading + merged continuation +
/// untranslated fallback (mirrors the captured oracle fixture).
fn parity_document() -> wenyi_core::document::models::Document {
    use wenyi_core::document::models::{Chapter, Document, Segment};
    let segment = |index: usize, source: &str, kind: &str, target: &str, cont: bool| Segment {
        index,
        source: source.to_string(),
        kind: kind.to_string(),
        target: if target.is_empty() {
            None
        } else {
            Some(target.to_string())
        },
        target_before_polish: None,
        anchor: None,
        resource_href: None,
        cont,
        meta: serde_json::Map::new(),
    };
    Document {
        title: "B".to_string(),
        source_lang: "en".to_string(),
        target_lang: "zh".to_string(),
        fmt: "text".to_string(),
        source_path: String::new(),
        chapters: vec![Chapter {
            index: 0,
            title: "Chapter One".to_string(),
            segments: vec![
                segment(0, "Chapter One", "heading", "第一章", false),
                segment(1, "The old Wolf prowls.", "text", "老狼潜行。", false),
                segment(2, " quietly.", "text", "悄然。", true),
                segment(3, "Moon rises.", "text", "", false),
            ],
            href: None,
            template: None,
            meta: serde_json::Map::new(),
        }],
        meta: serde_json::Map::new(),
    }
}

#[test]
fn text_writer_matches_oracle_bytes() {
    use wenyi_core::document::assemble::{assemble_text, AssembleOptions};
    let dir = temp_dir("export-parity");
    let document = parity_document();
    let mono = AssembleOptions::monolingual();
    let out = dir.join("o.txt");
    assemble_text(&document, &out, &mono, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "第一章\n\n老狼潜行。悄然。\n\nMoon rises.\n"
    );
    let bilingual = AssembleOptions {
        bilingual: true,
        order: "target_first".to_string(),
        preserve_source_style: false,
        preserve_source_ruby: None,
    };
    let out = dir.join("b.txt");
    assemble_text(&document, &out, &bilingual, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "第一章\n\n老狼潜行。悄然。\n\nThe old Wolf prowls. quietly.\n\nMoon rises.\n"
    );
    let source_first = AssembleOptions {
        bilingual: true,
        order: "source_first".to_string(),
        preserve_source_style: false,
        preserve_source_ruby: None,
    };
    let out = dir.join("o.md");
    assemble_text(&document, &out, &source_first, true).unwrap();
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "# 第一章\n\nThe old Wolf prowls. quietly.\n\n老狼潜行。悄然。\n\nMoon rises.\n"
    );
}

#[test]
fn html_writer_matches_oracle_structure() {
    use wenyi_core::document::assemble::{assemble_html, AssembleOptions};
    let dir = temp_dir("export-html");
    let document = parity_document();
    let bilingual = AssembleOptions {
        bilingual: true,
        order: "target_first".to_string(),
        preserve_source_style: false,
        preserve_source_ruby: None,
    };
    let out = dir.join("o.html");
    assemble_html(&document, &out, &bilingual).unwrap();
    let html = std::fs::read_to_string(&out).unwrap();
    // Byte-compare the body sequence (head carries the same CSS id).
    let body = html
        .split("<body>\n")
        .nth(1)
        .unwrap()
        .split("\n</body>")
        .next()
        .unwrap();
    assert_eq!(
        body,
        "<h1>第一章</h1><p>老狼潜行。悄然。</p><p class=\"tn-source\">The old Wolf prowls. quietly.</p><p>Moon rises.</p>"
    );
    assert!(html.contains("lang=\"zh-Hans\""));
    assert!(html.contains("tn-bilingual-style"));
}
