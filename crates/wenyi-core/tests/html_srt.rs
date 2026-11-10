//! HTML reader + SRT parser parity against oracle dumps.

use std::path::PathBuf;
use wenyi_core::document::{html, srt};

fn fixture(parts: &[&str]) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    for part in parts {
        path.push(part);
    }
    path
}

fn load_json(parts: &[&str]) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(fixture(parts)).unwrap()).unwrap()
}

#[test]
fn html_document_matches_oracle() {
    let doc = html::read_html(&fixture(&["html", "sample.html"]), "ja", "zh", None).unwrap();
    let mut got = serde_json::to_value(&doc).unwrap();
    got["source_path"] = serde_json::Value::String("<abs>/sample.html".to_string());
    let mut want = load_json(&["html", "sample.document.json"]);
    // chapter_tags comes from a Python set: order is unstable, compare as sets.
    let mut got_tags = got["meta"]["chapter_tags"].as_array().unwrap().clone();
    let mut want_tags = want["meta"]["chapter_tags"].as_array().unwrap().clone();
    got_tags.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    want_tags.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    assert_eq!(got_tags, want_tags, "chapter_tags");
    got["meta"].as_object_mut().unwrap().remove("chapter_tags");
    want["meta"].as_object_mut().unwrap().remove("chapter_tags");
    assert_eq!(got["meta"], want["meta"], "meta");
    assert_eq!(got["title"], want["title"], "title");
    assert_eq!(got["fmt"], want["fmt"], "fmt");
    let got_chapters = got["chapters"].as_array().unwrap();
    let want_chapters = want["chapters"].as_array().unwrap();
    assert_eq!(got_chapters.len(), want_chapters.len(), "chapter count");
    for (index, (got, want)) in got_chapters.iter().zip(want_chapters).enumerate() {
        assert_eq!(got["title"], want["title"], "ch{index} title");
        assert_eq!(got["segments"], want["segments"], "ch{index} segments");
        // Templates are serializer-specific; compare anchor placement.
        let got_anchors = anchor_ids(got["template"].as_str().unwrap());
        let want_templates = load_json(&["html", "sample.templates.json"]);
        let want_anchors = anchor_ids(want_templates[index].as_str().unwrap());
        assert_eq!(got_anchors, want_anchors, "ch{index} template anchors");
    }
}

#[test]
fn html_single_chapter_mode_keeps_all_segments() {
    // chapter_tags=[] disables splitting: every segment must survive in one
    // chapter (no silent chapter loss).
    let split = html::read_html(&fixture(&["html", "sample.html"]), "ja", "zh", None).unwrap();
    let single =
        html::read_html(&fixture(&["html", "sample.html"]), "ja", "zh", Some(&[])).unwrap();
    assert_eq!(single.chapters.len(), 1);
    let split_sources: Vec<&str> = split
        .chapters
        .iter()
        .flat_map(|c| c.segments.iter().map(|s| s.source.as_str()))
        .collect();
    let single_sources: Vec<&str> = single.chapters[0]
        .segments
        .iter()
        .map(|s| s.source.as_str())
        .collect();
    assert_eq!(single_sources, split_sources, "segment loss across modes");
}

#[test]
fn srt_cues_match_oracle() {
    let cues = srt::parse_srt(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("golden")
            .join("corpus")
            .join("three_cues.srt"),
    )
    .unwrap();
    let want = load_json(&["srt", "three_cues.json"]);
    assert_eq!(cues.len(), want.as_array().unwrap().len());
    for (cue, want) in cues.iter().zip(want.as_array().unwrap()) {
        assert_eq!(cue.index, want["index"].as_str().unwrap());
        assert_eq!(cue.timestamp, want["timestamp"].as_str().unwrap());
        assert_eq!(cue.text, want["text"].as_str().unwrap());
    }
}

#[test]
fn srt_rejects_files_without_valid_blocks() {
    let dir = std::env::temp_dir().join(format!("wenyi-srt-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bad = dir.join("bad.srt");
    std::fs::write(&bad, "just some text\nno cues here\n").unwrap();
    assert!(srt::parse_srt(&bad).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

fn anchor_ids(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(pos) = rest.find("data-tn-id") {
        rest = &rest[pos + 11..];
        let rest2 = rest.trim_start_matches(['=', ' ', '"', '\'']);
        let end = rest2.find(['"', '\'']).unwrap_or(rest2.len());
        out.push(rest2[..end].to_string());
        rest = &rest2[end..];
    }
    out
}
