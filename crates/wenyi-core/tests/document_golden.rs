//! Golden parity: token counts, source hashes, prefix budgets.
//!
//! Expectations in `tests/golden/snapshots/` were captured from the pinned
//! Python oracle (`tests/golden/capture.py`); this suite proves the Rust
//! implementations produce identical values.

use std::path::PathBuf;
use wenyi_core::document::{hash, tokens};

fn golden(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("golden")
        .join("snapshots")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn corpus(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("golden")
        .join("corpus")
        .join(name)
}

#[test]
fn token_counts_match_oracle() {
    let expected = golden("tokens.json");
    let texts = [
        ("empty", ""),
        (
            "ascii",
            "The old lighthouse stood at the edge of the cliff.",
        ),
        ("cjk", "吾輩は猫である。名前はまだ無い。"),
        ("mixed", "Keeper Elias Vance「灯台守」lit the lamp—again…"),
        ("whitespace", "  spaced\tout\n\nnewlines  "),
    ];
    for (key, text) in texts {
        assert_eq!(
            tokens::count_tokens(text),
            expected[key].as_u64().unwrap() as usize,
            "token mismatch for {key:?}"
        );
    }
}

#[test]
fn source_hashes_match_oracle() {
    let expected = golden("source_hashes.json");
    for name in ["two_chapters.txt", "headings.md", "three_cues.srt"] {
        assert_eq!(
            hash::source_sha256(&corpus(name)).unwrap(),
            expected[name].as_str().unwrap(),
            "hash mismatch for {name}"
        );
    }
}

#[test]
fn prefix_budget_matches_oracle_semantics() {
    // Oracle `_prefix_within_tokens`: fits → full length; empty/zero → 0;
    // otherwise largest fitting prefix with whitespace backtrack.
    let text = "The old lighthouse stood at the edge of the cliff. 吾輩は猫である。";
    assert_eq!(tokens::prefix_within_tokens(text, 10_000), text.len());
    assert_eq!(tokens::prefix_within_tokens("", 10), 0);
    assert_eq!(tokens::prefix_within_tokens(text, 0), 0);
    let end = tokens::prefix_within_tokens(text, 5);
    assert!(tokens::count_tokens(&text[..end]) <= 5);
    assert!(text.is_char_boundary(end));
    // Whitespace backtrack: oracle cuts AT the space (space stays in remainder).
    assert!(end == 0 || end == text.len() || text[end..].starts_with([' ', '\t', '\n']));
}

#[test]
fn text_reader_matches_oracle() {
    use wenyi_core::document::{segment, text, tokens};

    let expected = golden("segmentation.json");
    let doc = text::read_text(&corpus("two_chapters.txt"), "en", "zh").unwrap();
    let chapters = expected["chapters"].as_array().unwrap();
    assert_eq!(doc.chapters.len(), chapters.len());
    for (chapter, want) in doc.chapters.iter().zip(chapters) {
        assert_eq!(chapter.title, want["title"].as_str().unwrap());
        let segments = want["segments"].as_array().unwrap();
        // Oracle snapshot covers text_segments only; headings are included.
        let got: Vec<_> = chapter.segments.iter().collect();
        assert_eq!(got.len(), segments.len(), "segment count");
        for (segment, want) in got.iter().zip(segments) {
            assert_eq!(segment.index, want["index"].as_u64().unwrap() as usize);
            assert_eq!(segment.kind, want["kind"].as_str().unwrap());
            assert_eq!(
                segment.source.chars().count(),
                want["chars"].as_u64().unwrap() as usize
            );
            assert_eq!(
                tokens::count_tokens(&segment.source),
                want["tokens"].as_u64().unwrap() as usize
            );
        }
    }
    // Batch grouping parity for the first chapter.
    let batches = segment::batch_segments(&doc.chapters[0].segments, 1800);
    let want_batches = expected["first_chapter_batches_1800"].as_array().unwrap();
    assert_eq!(batches.len(), want_batches.len());
    for (batch, want) in batches.iter().zip(want_batches) {
        let got: Vec<usize> = batch.iter().map(|s| s.index).collect();
        let want: Vec<usize> = want
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as usize)
            .collect();
        assert_eq!(got, want);
    }
}
