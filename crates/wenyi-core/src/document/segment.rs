//! Segmentation: long-paragraph splits and token batching.
//!
//! Oracle: `ingest/segmenter.py` (`split_long_segments`, `batch_segments`).

use crate::document::models::{Chapter, Segment, KIND_TEXT};
use crate::document::tokens::{count_tokens, prefix_within_tokens};

/// Sentence-boundary characters (oracle `_SENT_SPLIT` lookbehind set).
const SENTENCE_ENDS: [char; 9] = ['。', '．', '.', '!', '！', '？', '?', '…', '\n'];

/// Split `text` after every sentence-ending character (lookbehind semantics).
fn split_sentences(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if SENTENCE_ENDS.contains(&c) {
            parts.push(&text[start..i + c.len_utf8()]);
            start = i + c.len_utf8();
        }
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    if parts.is_empty() {
        parts.push(text);
    }
    parts
}

/// Hard-split an oversized sentence into token-fitting chunks.
fn split_oversized_sentence(text: &str, max_tokens: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut rest = text;
    while count_tokens(rest) > max_tokens {
        let mut cut = prefix_within_tokens(rest, max_tokens);
        if cut == 0 {
            // Minimum 1 char of progress (oracle: `cut = 1`).
            cut = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        }
        chunks.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    if !rest.is_empty() {
        chunks.push(rest.to_string());
    }
    chunks
}

/// Greedily group sentence parts into token-fitting chunks (oracle `_split_text`).
fn split_text(text: &str, max_tokens: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_tokens = 0usize;
    for part in split_sentences(text) {
        let part_tokens = count_tokens(part);
        if part_tokens > max_tokens {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                current_tokens = 0;
            }
            chunks.extend(split_oversized_sentence(part, max_tokens));
        } else if !current.is_empty() && current_tokens + part_tokens > max_tokens {
            chunks.push(std::mem::take(&mut current));
            current.push_str(part);
            current_tokens = part_tokens;
        } else {
            current.push_str(part);
            current_tokens += part_tokens;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(text.to_string());
    }
    chunks
}

/// Split over-long segments in place; re-index `0..` (oracle `split_long_segments`).
///
/// No-op when `max_tokens == 0`. First piece keeps kind/anchor/meta;
/// continuations are `text` kind, `cont = true`, anchorless, metaless.
pub fn split_long_segments(chapters: &mut [Chapter], max_tokens: usize) {
    if max_tokens == 0 {
        return;
    }
    for chapter in chapters {
        let mut rebuilt: Vec<Segment> = Vec::with_capacity(chapter.segments.len());
        for segment in std::mem::take(&mut chapter.segments) {
            if count_tokens(&segment.source) <= max_tokens {
                rebuilt.push(segment);
                continue;
            }
            let mut pieces = split_text(&segment.source, max_tokens).into_iter();
            if let Some(first) = pieces.next() {
                let mut head = segment;
                head.source = first;
                head.cont = false;
                rebuilt.push(head);
                let template = rebuilt.last().unwrap();
                let resource_href = template.resource_href.clone();
                for piece in pieces {
                    rebuilt.push(Segment {
                        index: 0,
                        source: piece,
                        kind: KIND_TEXT.to_string(),
                        target: None,
                        target_before_polish: None,
                        anchor: None,
                        resource_href: resource_href.clone(),
                        cont: true,
                        meta: serde_json::Map::new(),
                    });
                }
            }
        }
        for (index, segment) in rebuilt.iter_mut().enumerate() {
            segment.index = index;
        }
        chapter.segments = rebuilt;
    }
}

/// Group segments into token-fitting batches (oracle `batch_segments`).
///
/// A single oversized segment gets its own batch. Empty input yields no batches.
pub fn batch_segments(segments: &[Segment], max_tokens: usize) -> Vec<Vec<&Segment>> {
    let mut batches: Vec<Vec<&Segment>> = Vec::new();
    let mut current: Vec<&Segment> = Vec::new();
    let mut current_tokens = 0usize;
    for segment in segments {
        let length = count_tokens(&segment.source);
        if !current.is_empty() && current_tokens + length > max_tokens {
            batches.push(std::mem::take(&mut current));
            current_tokens = 0;
        }
        current.push(segment);
        current_tokens += length;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}
