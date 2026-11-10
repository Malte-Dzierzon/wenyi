//! Segmentation behavior parity with the oracle's `TestSplitLongSegments`.

use wenyi_core::document::models::{Chapter, Segment, KIND_HEADING, KIND_TEXT};
use wenyi_core::document::segment::{batch_segments, split_long_segments};
use wenyi_core::document::tokens::count_tokens;

fn chapter(segments: Vec<Segment>) -> Chapter {
    Chapter {
        index: 0,
        title: "test".to_string(),
        segments,
        href: None,
        template: None,
        meta: serde_json::Map::new(),
    }
}

fn segment(index: usize, source: &str, kind: &str, anchor: Option<&str>) -> Segment {
    Segment {
        index,
        source: source.to_string(),
        kind: kind.to_string(),
        target: None,
        target_before_polish: None,
        anchor: anchor.map(str::to_string),
        resource_href: None,
        cont: false,
        meta: serde_json::Map::new(),
    }
}

#[test]
fn split_by_sentence_and_cont_flag() {
    let long_src = "第一句。".repeat(10); // Fifty tokens under cl100k_base.
    let mut chapters = vec![chapter(vec![
        segment(0, "标题", KIND_HEADING, Some("a0")),
        segment(1, &long_src, KIND_TEXT, Some("a1")),
        segment(2, "短。", KIND_TEXT, Some("a2")),
    ])];
    split_long_segments(&mut chapters, 30);
    let segments = &chapters[0].segments;
    assert!(segments.iter().any(|s| s.cont));
    let heads: Vec<_> = segments
        .iter()
        .filter(|s| !s.cont && s.anchor.as_deref() == Some("a1"))
        .collect();
    assert_eq!(heads.len(), 1);
    assert!(segments
        .iter()
        .filter(|s| s.cont)
        .all(|s| s.anchor.is_none()));
    let indices: Vec<usize> = segments.iter().map(|s| s.index).collect();
    assert_eq!(indices, (0..segments.len()).collect::<Vec<_>>());
    let joined: String = segments
        .iter()
        .filter(|s| s.anchor.as_deref() == Some("a1") || s.cont)
        .map(|s| s.source.as_str())
        .collect();
    assert_eq!(joined, long_src);
}

#[test]
fn split_keeps_meta_only_on_first_part() {
    let mut meta = serde_json::Map::new();
    meta.insert("k".to_string(), serde_json::Value::from(1));
    let mut original = segment(0, &"第一句。".repeat(10), KIND_TEXT, Some("a0"));
    original.meta = meta;
    let mut chapters = vec![chapter(vec![original])];
    split_long_segments(&mut chapters, 20);
    let segments = &chapters[0].segments;
    assert!(segments.len() > 1);
    assert!(!segments[0].meta.is_empty());
    assert!(segments[1..].iter().all(|s| s.meta.is_empty()));
}

#[test]
fn no_split_when_short() {
    let mut chapters = vec![chapter(vec![segment(0, "短句。", KIND_TEXT, Some("a0"))])];
    split_long_segments(&mut chapters, 100);
    assert_eq!(chapters[0].segments.len(), 1);
    assert!(!chapters[0].segments[0].cont);
}

#[test]
fn oversized_single_sentence_hard_splits_losslessly() {
    let text = "あ".repeat(50);
    let mut chapters = vec![chapter(vec![segment(0, &text, KIND_TEXT, None)])];
    split_long_segments(&mut chapters, 20);
    let segments = &chapters[0].segments;
    assert!(segments.iter().all(|s| count_tokens(&s.source) <= 20));
    let joined: String = segments.iter().map(|s| s.source.as_str()).collect();
    assert_eq!(joined, text);
}

#[test]
fn batching_uses_token_counts() {
    let segments = vec![
        segment(0, "Alpha beta gamma. Delta epsilon zeta!", KIND_TEXT, None),
        segment(1, "短。", KIND_TEXT, None),
    ];
    // Tiny budget forces one batch per segment.
    let batches = batch_segments(&segments, 1);
    assert_eq!(batches.len(), 2);
    // Generous budget groups both.
    let batches = batch_segments(&segments, 10_000);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 2);
    // Empty input yields no batches.
    assert!(batch_segments(&[], 100).is_empty());
}
