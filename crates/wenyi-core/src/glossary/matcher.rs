//! Glossary term matching. Oracle: `glossary/store.py` (matcher half).
//!
//! NFKC + casefold normalization, ruby-marker stripping, word-boundary
//! matching for space-delimited scripts, substring matching for CJK.
//! The store half (SQLite tables) lands with the Slice 4 migration.

use unicode_normalization::UnicodeNormalization;

/// Source-only match types (oracle `_SOURCE_ONLY_TYPES`).
pub const SOURCE_ONLY_TYPES: [&str; 4] = ["appellation", "honorific", "speech", "fixed_expression"];

/// Normalize width, compatibility forms and case (oracle `_match_text`).
pub fn match_text(text: &str) -> String {
    let stripped = if text.contains('〘') {
        strip_ruby_marks(text)
    } else {
        text.to_string()
    };
    stripped.nfkc().collect::<String>().to_lowercase()
}

fn strip_ruby_marks(text: &str) -> String {
    crate::document::ruby::strip_markers(text)
}

/// Normalize one match key; empty means unmatchable (oracle
/// `normalize_match_key`).
pub fn normalize_match_key(source: &str) -> String {
    match_text(source).trim().to_string()
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether a normalized key matches inside normalized text with oracle
/// boundary semantics (oracle `_matches_normalized` + `_source_pattern`).
pub fn matches_normalized(key: &str, normalized_text: &str) -> bool {
    if key.is_empty() || !normalized_text.contains(key) {
        return false;
    }
    match boundary_kind(key) {
        BoundaryKind::Ascii => {
            // `(?<![a-z0-9_])key(?![a-z0-9_])`: key is already lowercased.
            let bytes = normalized_text.as_bytes();
            let needle = key.as_bytes();
            let mut start = 0;
            while let Some(pos) = find_subslice(&bytes[start..], needle) {
                let at = start + pos;
                let before_ok = at == 0 || !is_ascii_word(bytes[at - 1]);
                let after = at + needle.len();
                let after_ok = after >= bytes.len() || !is_ascii_word(bytes[after]);
                if before_ok && after_ok {
                    return true;
                }
                start = at + 1;
            }
            false
        }
        BoundaryKind::Word { left, right } => {
            let chars: Vec<char> = normalized_text.chars().collect();
            let needle: Vec<char> = key.chars().collect();
            if needle.len() > chars.len() {
                return false;
            }
            for at in 0..=chars.len() - needle.len() {
                if chars[at..at + needle.len()] != needle[..] {
                    continue;
                }
                let before_ok = !left || at == 0 || !is_word_char(chars[at - 1]);
                let after = at + needle.len();
                let after_ok = !right || after >= chars.len() || !is_word_char(chars[after]);
                if before_ok && after_ok {
                    return true;
                }
            }
            false
        }
        BoundaryKind::Substring => true,
    }
}
/// Whether a key occurrence at byte offsets `[at, end)` satisfies the
/// oracle boundary rules (oracle `_source_occurrence_spans`).
pub fn matches_normalized_at(key: &str, normalized_text: &str, at: usize, end: usize) -> bool {
    if key.is_empty() || end > normalized_text.len() || at >= end {
        return false;
    }
    match boundary_kind(key) {
        BoundaryKind::Ascii => {
            let bytes = normalized_text.as_bytes();
            let before_ok = at == 0 || !is_ascii_word(bytes[at - 1]);
            let after_ok = end >= bytes.len() || !is_ascii_word(bytes[end]);
            before_ok && after_ok
        }
        BoundaryKind::Word { left, right } => {
            let before_ok = !left
                || normalized_text[..at]
                    .chars()
                    .last()
                    .is_none_or(|c| !is_word_char(c));
            let after_ok = !right
                || normalized_text[end..]
                    .chars()
                    .next()
                    .is_none_or(|c| !is_word_char(c));
            before_ok && after_ok
        }
        BoundaryKind::Substring => true,
    }
}

fn is_ascii_word(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

enum BoundaryKind {
    Ascii,
    Word { left: bool, right: bool },
    Substring,
}

fn boundary_kind(key: &str) -> BoundaryKind {
    if key.is_ascii() {
        return BoundaryKind::Ascii;
    }
    let letters: Vec<char> = key.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() || !letters.iter().all(|c| is_boundary_script(*c)) {
        return BoundaryKind::Substring;
    }
    let first = key.chars().next().unwrap_or(' ');
    let last = key.chars().last().unwrap_or(' ');
    BoundaryKind::Word {
        left: first.is_alphanumeric(),
        right: last.is_alphanumeric(),
    }
}

/// Latin/Greek/Cyrillic check without a Unicode-data dependency: the
/// oracle consults `unicodedata.name`; Slice 4 covers the same scripts
/// via block ranges.
fn is_boundary_script(c: char) -> bool {
    let v = c as u32;
    matches!(
        v,
        0x0041..=0x024F
            | 0x1E00..=0x1EFF
            | 0x2C60..=0x2C7F
            | 0xA720..=0xA7FF
            | 0xAB30..=0xAB6F
            | 0x0370..=0x03FF
            | 0x1F00..=0x1FFF
            | 0x0400..=0x04FF
            | 0x0500..=0x052F
            | 0x2DE0..=0x2DFF
            | 0xA640..=0xA69F
            | 0x1C80..=0x1C8F
    )
}

/// Match raw keys against pre-normalized text (oracle
/// `sources_match_normalized`).
pub fn sources_match_normalized(keys: &[String], normalized_text: &str) -> bool {
    keys.iter().any(|source| {
        let key = normalize_match_key(source);
        !key.is_empty() && matches_normalized(&key, normalized_text)
    })
}

/// Match one source term against text (oracle `source_matches_text`).
pub fn source_matches_text(source: &str, text: &str) -> bool {
    let key = normalize_match_key(source);
    if key.is_empty() {
        return false;
    }
    matches_normalized(&key, &match_text(text))
}

/// One term prepared for repeated matching (oracle `TermMatcher`).
#[derive(Debug, Default)]
pub struct TermMatcher {
    prepared: Vec<(String, String)>,
}

impl TermMatcher {
    /// Prepare raw source keys (source + allowed aliases).
    pub fn new(keys: &[String]) -> Self {
        let mut prepared = Vec::new();
        for source in keys {
            let key = normalize_match_key(source);
            if !key.is_empty() {
                prepared.push((source.clone(), key));
            }
        }
        Self { prepared }
    }

    /// Whether any usable key exists.
    pub fn is_empty(&self) -> bool {
        self.prepared.is_empty()
    }

    /// First raw key matching text (oracle `first_hit`).
    pub fn first_hit(&self, text: &str) -> Option<&str> {
        let normalized = match_text(text);
        for (source, key) in &self.prepared {
            if !normalized.contains(key) {
                continue;
            }
            if matches_normalized(key, &normalized) {
                return Some(source);
            }
        }
        None
    }

    /// Whether any key matches text.
    pub fn matches(&self, text: &str) -> bool {
        self.first_hit(text).is_some()
    }
}

/// Multi-pattern candidate index over one glossary snapshot.
///
/// An Aho-Corasick automaton over normalized keys finds candidate spans in
/// one pass; every candidate is re-verified with
/// [`matches_normalized_at`] (oracle boundary semantics). The index is a
/// candidate filter, never the correctness rule.
#[derive(Debug, Default)]
pub struct CandidateIndex {
    /// Normalized key per pattern id.
    keys: Vec<String>,
    /// Term row index per pattern id.
    owners: Vec<usize>,
    /// Automaton over `keys`, if any.
    automaton: Option<aho_corasick::AhoCorasick>,
}

impl CandidateIndex {
    /// Build over `(term_index, key)` pairs (raw keys, normalized here).
    pub fn build(entries: &[(usize, String)]) -> Self {
        let mut keys = Vec::new();
        let mut owners = Vec::new();
        for (owner, raw) in entries {
            let key = normalize_match_key(raw);
            if key.is_empty() {
                continue;
            }
            keys.push(key);
            owners.push(*owner);
        }
        let automaton = if keys.is_empty() {
            None
        } else {
            aho_corasick::AhoCorasick::new(&keys).ok()
        };
        Self {
            keys,
            owners,
            automaton,
        }
    }

    /// Verified `(term_index, matched_key, byte_start, byte_end)` hits.
    pub fn find_verified(&self, text: &str) -> Vec<(usize, String, usize, usize)> {
        let Some(automaton) = &self.automaton else {
            return Vec::new();
        };
        let normalized = match_text(text);
        let mut out = Vec::new();
        for hit in automaton.find_iter(&normalized) {
            let key = &self.keys[hit.pattern().as_usize()];
            if matches_normalized_at(key, &normalized, hit.start(), hit.end()) {
                out.push((
                    self.owners[hit.pattern().as_usize()],
                    key.clone(),
                    hit.start(),
                    hit.end(),
                ));
            }
        }
        out
    }
}
