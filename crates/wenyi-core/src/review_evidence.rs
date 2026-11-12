//! Read-only whole-book evidence index for review agents.
//!
//! Oracle: `review/evidence.py`. Shadow-aware segment view with four
//! bounded query tools (`glossary_term`, `term_occurrences`,
//! `segment_context`, `book_context`). Results cap at 64 KiB; refs feed
//! the agent loop's allowlist.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::glossary::{store::term_match_sources, GlossaryTerm, TermMatcher};
use crate::review_models::{normalize_value, SegmentRef};

/// Single result byte cap (oracle `execute`).
pub const EVIDENCE_RESULT_LIMIT: usize = 64_000;

/// Bound glossary field lengths within evidence messages.
fn clip(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

/// Stable glossary reference (oracle `_glossary_ref`).
pub fn glossary_ref(source: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    format!(
        "glossary:{:.16}",
        crate::document::hash::hex_of(&hasher.finalize())
    )
}

/// Read-only whole-book evidence index (oracle `BookEvidenceIndex`).
pub struct BookEvidenceIndex {
    /// Flattened shadow-aware segments in book order.
    pub segments: Vec<SegmentRef>,
    /// Location → position.
    by_location: BTreeMap<(i64, i64), usize>,
    /// Glossary snapshot.
    pub terms: Vec<GlossaryTerm>,
    /// Book analysis (style guide + synopsis).
    pub analysis: serde_json::Map<String, serde_json::Value>,
    /// Chapter digests by chapter index.
    pub chapter_digests: BTreeMap<i64, String>,
    /// Source language (readings only for `ja`).
    pub source_lang: String,
    /// Exact source → term position.
    exact_source: BTreeMap<String, usize>,
    /// Normalized source → term positions.
    source_lookup: BTreeMap<String, Vec<usize>>,
    /// Normalized alias → term positions.
    alias_lookup: BTreeMap<String, Vec<usize>>,
    /// Occurrence cache by `term:`/`literal:` key.
    occurrence_cache: BTreeMap<String, Vec<usize>>,
}

/// One chapter's reviewable segments for index construction.
pub struct EvidenceChapter {
    /// Chapter index.
    pub index: i64,
    /// Chapter title.
    pub title: String,
    /// Source digest.
    pub digest: String,
    /// (text_index, segment_index, source, formal_target).
    pub segments: Vec<(i64, i64, String, String)>,
}

impl BookEvidenceIndex {
    /// Build over chapters + terms + overrides (oracle `__init__`).
    pub fn new(
        chapters: &[EvidenceChapter],
        terms: Vec<GlossaryTerm>,
        analysis: serde_json::Map<String, serde_json::Value>,
        source_lang: &str,
        overrides: &BTreeMap<(i64, i64), String>,
    ) -> Self {
        let mut segments = Vec::new();
        let mut by_location = BTreeMap::new();
        let mut chapter_digests = BTreeMap::new();
        for chapter in chapters {
            chapter_digests.insert(chapter.index, chapter.digest.clone());
            for (text_index, segment_index, source, baseline) in &chapter.segments {
                let location = (chapter.index, *text_index);
                let position = segments.len();
                let has_override = overrides.contains_key(&location);
                segments.push(SegmentRef {
                    global_ordinal: position as i64,
                    chapter: chapter.index,
                    text_index: *text_index,
                    segment_index: *segment_index,
                    source: source.clone(),
                    target: overrides
                        .get(&location)
                        .cloned()
                        .unwrap_or_else(|| baseline.clone()),
                    baseline_target: baseline.clone(),
                    target_origin: if has_override {
                        "shadow_override".to_string()
                    } else {
                        "formal".to_string()
                    },
                    chapter_title: chapter.title.clone(),
                });
                by_location.insert(location, position);
            }
        }
        let mut exact_source = BTreeMap::new();
        let mut source_lookup: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut alias_lookup: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (position, term) in terms.iter().enumerate() {
            exact_source.insert(term.source.clone(), position);
            let normalized = normalize_value(&term.source);
            if !normalized.is_empty() {
                source_lookup.entry(normalized).or_default().push(position);
            }
            for alias in &term.aliases {
                let normalized = normalize_value(alias);
                if !normalized.is_empty() {
                    alias_lookup.entry(normalized).or_default().push(position);
                }
            }
        }
        Self {
            segments,
            by_location,
            terms,
            analysis,
            chapter_digests,
            source_lang: source_lang.to_string(),
            exact_source,
            source_lookup,
            alias_lookup,
            occurrence_cache: BTreeMap::new(),
        }
    }

    /// Paragraph reference by chapter + text index.
    pub fn segment_ref(&self, chapter: i64, text_index: i64) -> Option<&SegmentRef> {
        self.by_location
            .get(&(chapter, text_index))
            .map(|pos| &self.segments[*pos])
    }

    /// Resolve a source/alias to a canonical term position + candidates.
    pub fn canonical_term(&self, query: &str) -> (Option<usize>, Vec<String>) {
        let stripped = query.trim();
        if let Some(position) = self.exact_source.get(stripped) {
            return (Some(*position), Vec::new());
        }
        let normalized = normalize_value(stripped);
        let unique_sources: BTreeSet<&String> = self
            .source_lookup
            .get(&normalized)
            .map(|positions| {
                positions
                    .iter()
                    .map(|pos| &self.terms[*pos].source)
                    .collect()
            })
            .unwrap_or_default();
        if unique_sources.len() == 1 {
            let source = unique_sources.into_iter().next().unwrap();
            let position = self.exact_source.get(source).copied();
            return (position, Vec::new());
        }
        if unique_sources.len() > 1 {
            return (None, unique_sources.into_iter().cloned().collect());
        }
        let unique: BTreeSet<&String> = self
            .alias_lookup
            .get(&normalized)
            .map(|positions| {
                positions
                    .iter()
                    .map(|pos| &self.terms[*pos].source)
                    .collect()
            })
            .unwrap_or_default();
        if unique.len() == 1 {
            let source = unique.into_iter().next().unwrap();
            let position = self.exact_source.get(source).copied();
            return (position, Vec::new());
        }
        if unique.len() > 1 {
            return (None, unique.into_iter().cloned().collect());
        }
        (None, Vec::new())
    }

    /// Scan and cache matching paragraphs (oracle `_occurrences`).
    fn occurrences(&mut self, query: &str) -> (String, Vec<usize>, Vec<String>) {
        let (term, ambiguous) = self.canonical_term(query);
        if !ambiguous.is_empty() {
            return (String::new(), Vec::new(), ambiguous);
        }
        let canonical = term
            .map(|pos| self.terms[pos].source.clone())
            .unwrap_or_else(|| query.trim().to_string());
        if canonical.is_empty() {
            return (String::new(), Vec::new(), Vec::new());
        }
        let cache_key = match term {
            Some(pos) => format!("term:{}", self.terms[pos].source),
            None => format!("literal:{}", normalize_value(&canonical)),
        };
        if let Some(cached) = self.occurrence_cache.get(&cache_key) {
            return (canonical, cached.clone(), Vec::new());
        }
        let keys = term
            .map(|pos| term_match_sources(&self.terms[pos]))
            .unwrap_or_else(|| vec![canonical.clone()]);
        let matcher = TermMatcher::new(&keys);
        let found: Vec<usize> = self
            .segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| matcher.matches(&segment.source))
            .map(|(position, _)| position)
            .collect();
        self.occurrence_cache.insert(cache_key, found.clone());
        (canonical, found, Vec::new())
    }

    /// Convert selectors to deduped zero-based positions (oracle
    /// `_selector_positions`).
    fn selector_positions(
        selectors: &[serde_json::Value],
        total: usize,
    ) -> (Vec<usize>, Vec<serde_json::Value>) {
        let mut positions = Vec::new();
        let mut invalid = Vec::new();
        for selector in selectors.iter().take(8) {
            let mut position: Option<i64> = None;
            match selector {
                serde_json::Value::Number(n) if n.as_i64().is_some() => {
                    position = Some(n.as_i64().unwrap() - 1);
                }
                serde_json::Value::String(text) if text == "first" => position = Some(0),
                serde_json::Value::String(text) if text == "middle" => {
                    position = if total > 0 {
                        Some(((total - 1) / 2) as i64)
                    } else {
                        None
                    };
                }
                serde_json::Value::String(text) if text == "last" => {
                    position = if total > 0 {
                        Some((total - 1) as i64)
                    } else {
                        None
                    };
                }
                _ => invalid.push(selector.clone()),
            }
            match position {
                Some(pos) if pos >= 0 && (pos as usize) < total => {
                    if !positions.contains(&(pos as usize)) {
                        positions.push(pos as usize);
                    }
                }
                _ => {
                    if !invalid.contains(selector) {
                        invalid.push(selector.clone());
                    }
                }
            }
        }
        (positions, invalid)
    }

    /// Cross-chapter context in book order (oracle `_context`).
    fn context(&self, position: usize, before: usize, after: usize) -> Vec<serde_json::Value> {
        let start = position.saturating_sub(before);
        let end = (position + after + 1).min(self.segments.len());
        self.segments[start..end]
            .iter()
            .map(|segment| serde_json::Value::Object(segment.compact()))
            .collect()
    }

    /// Condense one term into citable evidence (oracle `_term_evidence`).
    fn term_evidence(&self, term: &GlossaryTerm) -> serde_json::Map<String, serde_json::Value> {
        let mut out = serde_json::Map::new();
        out.insert("ref".to_string(), json!(glossary_ref(&term.source)));
        out.insert("source".to_string(), json!(clip(&term.source, 256)));
        out.insert("target".to_string(), json!(clip(&term.target, 256)));
        if self.source_lang == "ja" {
            out.insert("reading".to_string(), json!(clip(&term.reading, 256)));
        }
        out.insert("type".to_string(), json!(clip(&term.term_type, 64)));
        out.insert("gender".to_string(), json!(clip(&term.gender, 64)));
        out.insert(
            "aliases".to_string(),
            json!(term
                .aliases
                .iter()
                .take(16)
                .map(|a| clip(a, 256))
                .collect::<Vec<_>>()),
        );
        out.insert("first_chapter".to_string(), json!(term.first_chapter));
        out.insert("note".to_string(), json!(clip(&term.note, 2000)));
        out
    }

    /// Selected occurrence paragraphs for one term (oracle
    /// `term_occurrences`).
    pub fn term_occurrences(
        &mut self,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut out = serde_json::Map::new();
        let query = arguments
            .get("term")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if query.trim().is_empty() || query.chars().count() > 128 {
            out.insert("ok".to_string(), json!(false));
            out.insert("error".to_string(), json!("invalid_term"));
            return out;
        }
        let selectors = arguments.get("selectors").and_then(|v| v.as_array());
        let default_selectors = vec![json!("first"), json!("middle"), json!("last")];
        let selectors = selectors.map_or(default_selectors.as_slice(), |v| v.as_slice());
        if selectors.is_empty() || selectors.len() > 8 {
            out.insert("ok".to_string(), json!(false));
            out.insert("error".to_string(), json!("invalid_selectors"));
            return out;
        }
        let radius = arguments
            .get("context_radius")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if !(0..=2).contains(&radius) {
            out.insert("ok".to_string(), json!(false));
            out.insert("error".to_string(), json!("invalid_context_radius"));
            return out;
        }
        // Bool guard: serde bools are not i64, so `true` already fell to 0;
        // reject explicit booleans like the oracle.
        if arguments
            .get("context_radius")
            .is_some_and(|v| v.is_boolean())
        {
            out.insert("ok".to_string(), json!(false));
            out.insert("error".to_string(), json!("invalid_context_radius"));
            return out;
        }
        let (term, _) = self.canonical_term(query);
        let (canonical, occurrences, ambiguous) = self.occurrences(query);
        if !ambiguous.is_empty() {
            out.insert("ok".to_string(), json!(false));
            out.insert("error".to_string(), json!("ambiguous_term"));
            out.insert("candidates".to_string(), json!(ambiguous));
            return out;
        }
        let (positions, invalid) = Self::selector_positions(selectors, occurrences.len());
        let mut selected = Vec::new();
        for position in positions {
            let segment = &self.segments[occurrences[position]];
            let mut entry = segment.compact();
            entry.insert("ordinal".to_string(), json!(position as i64 + 1));
            entry.insert(
                "context".to_string(),
                serde_json::Value::Array(self.context(
                    segment.global_ordinal as usize,
                    radius as usize,
                    radius as usize,
                )),
            );
            selected.push(serde_json::Value::Object(entry));
        }
        out.insert("ok".to_string(), json!(true));
        out.insert("canonical_term".to_string(), json!(canonical));
        out.insert(
            "glossary_term".to_string(),
            term.map(|pos| serde_json::Value::Object(self.term_evidence(&self.terms[pos].clone())))
                .unwrap_or(serde_json::Value::Null),
        );
        out.insert("total_matches".to_string(), json!(occurrences.len() as i64));
        out.insert("selected".to_string(), serde_json::Value::Array(selected));
        out.insert(
            "invalid_selectors".to_string(),
            serde_json::Value::Array(invalid),
        );
        out
    }

    /// One canonical term by source or alias (oracle `glossary_term`).
    pub fn glossary_term(
        &self,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut out = serde_json::Map::new();
        let query = arguments
            .get("term")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if query.trim().is_empty() || query.chars().count() > 128 {
            out.insert("ok".to_string(), json!(false));
            out.insert("error".to_string(), json!("invalid_term"));
            return out;
        }
        let (term, ambiguous) = self.canonical_term(query);
        if !ambiguous.is_empty() {
            out.insert("ok".to_string(), json!(false));
            out.insert("error".to_string(), json!("ambiguous_term"));
            out.insert("candidates".to_string(), json!(ambiguous));
            return out;
        }
        let Some(position) = term else {
            out.insert("ok".to_string(), json!(false));
            out.insert("error".to_string(), json!("term_not_found"));
            return out;
        };
        out.insert("ok".to_string(), json!(true));
        out.insert(
            "term".to_string(),
            serde_json::Value::Object(self.term_evidence(&self.terms[position])),
        );
        out
    }

    /// Source/target context around a paragraph (oracle `segment_context`).
    pub fn segment_context(
        &self,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut out = serde_json::Map::new();
        let int = |key: &str| arguments.get(key).and_then(|v| v.as_i64());
        let is_bool = |key: &str| arguments.get(key).is_some_and(|v| v.is_boolean());
        if is_bool("chapter") || is_bool("index") || is_bool("before") || is_bool("after") {
            out.insert("ok".to_string(), json!(false));
            out.insert(
                "error".to_string(),
                json!("invalid_segment_context_arguments"),
            );
            return out;
        }
        let (Some(chapter), Some(text_index)) = (int("chapter"), int("index")) else {
            out.insert("ok".to_string(), json!(false));
            out.insert(
                "error".to_string(),
                json!("invalid_segment_context_arguments"),
            );
            return out;
        };
        let before = int("before").unwrap_or(2);
        let after = int("after").unwrap_or(2);
        if !(0..=6).contains(&before) || !(0..=6).contains(&after) {
            out.insert("ok".to_string(), json!(false));
            out.insert("error".to_string(), json!("context_limit_exceeded"));
            return out;
        }
        let Some(position) = self.by_location.get(&(chapter, text_index)) else {
            out.insert("ok".to_string(), json!(false));
            out.insert("error".to_string(), json!("segment_not_found"));
            return out;
        };
        out.insert("ok".to_string(), json!(true));
        out.insert(
            "center".to_string(),
            json!(self.segments[*position].reference()),
        );
        out.insert(
            "segments".to_string(),
            serde_json::Value::Array(self.context(*position, before as usize, after as usize)),
        );
        out
    }

    /// Bounded book/chapter analysis (oracle `book_context`).
    pub fn book_context(
        &self,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut out = serde_json::Map::new();
        let section = arguments
            .get("section")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let value = match section {
            "style_guide" => self.analysis.get("style_guide"),
            "book_synopsis" => self.analysis.get("book_synopsis"),
            "chapter_digest" => {
                let chapter = arguments.get("chapter").and_then(|v| v.as_i64());
                let is_bool = arguments.get("chapter").is_some_and(|v| v.is_boolean());
                if is_bool || chapter.is_none() {
                    out.insert("ok".to_string(), json!(false));
                    out.insert("error".to_string(), json!("invalid_chapter"));
                    return out;
                }
                let chapter = chapter.unwrap();
                if !self.chapter_digests.contains_key(&chapter) {
                    out.insert("ok".to_string(), json!(false));
                    out.insert("error".to_string(), json!("chapter_not_found"));
                    return out;
                }
                let text = self
                    .chapter_digests
                    .get(&chapter)
                    .cloned()
                    .unwrap_or_default();
                return Self::book_value(
                    section,
                    &text,
                    &format!("book:chapter_digest:ch{chapter}"),
                );
            }
            _ => {
                out.insert("ok".to_string(), json!(false));
                out.insert("error".to_string(), json!("invalid_book_context_section"));
                return out;
            }
        };
        let text = match value {
            Some(serde_json::Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        };
        Self::book_value(section, &text, &format!("book:{section}"))
    }

    /// Clip one book value to 6000 chars with a ref.
    fn book_value(
        section: &str,
        text: &str,
        reference: &str,
    ) -> serde_json::Map<String, serde_json::Value> {
        const LIMIT: usize = 6000;
        let mut out = serde_json::Map::new();
        out.insert("ok".to_string(), json!(true));
        out.insert("ref".to_string(), json!(reference));
        out.insert("section".to_string(), json!(section));
        out.insert(
            "value".to_string(),
            json!(text.chars().take(LIMIT).collect::<String>()),
        );
        out.insert("truncated".to_string(), json!(text.chars().count() > LIMIT));
        out
    }

    /// Validate and execute one JSON evidence request (oracle `execute`).
    pub fn execute(
        &mut self,
        request: &serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut out = serde_json::Map::new();
        let request_id = request
            .get("request_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if request_id.trim().is_empty() {
            out.insert("request_id".to_string(), json!(""));
            out.insert("ok".to_string(), json!(false));
            out.insert("error".to_string(), json!("invalid_request_id"));
            return out;
        }
        let Some(arguments) = request.get("arguments").and_then(|v| v.as_object()) else {
            out.insert("request_id".to_string(), json!(request_id));
            out.insert("ok".to_string(), json!(false));
            out.insert("error".to_string(), json!("invalid_arguments"));
            return out;
        };
        let tool = request
            .get("tool")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let mut result = match tool {
            "glossary_term" => self.glossary_term(arguments),
            "term_occurrences" => self.term_occurrences(arguments),
            "segment_context" => self.segment_context(arguments),
            "book_context" => self.book_context(arguments),
            _ => {
                out.insert("request_id".to_string(), json!(request_id));
                out.insert(
                    "tool".to_string(),
                    request.get("tool").cloned().unwrap_or(json!(tool)),
                );
                out.insert("ok".to_string(), json!(false));
                out.insert("error".to_string(), json!("unknown_tool"));
                return out;
            }
        };
        result.insert("request_id".to_string(), json!(request_id));
        result.insert("tool".to_string(), json!(tool));
        if serde_json::Value::Object(result.clone()).to_string().len() > EVIDENCE_RESULT_LIMIT {
            let mut too_large = serde_json::Map::new();
            too_large.insert("request_id".to_string(), json!(request_id));
            too_large.insert("tool".to_string(), json!(tool));
            too_large.insert("ok".to_string(), json!(false));
            too_large.insert("error".to_string(), json!("evidence_result_too_large"));
            too_large.insert(
                "hint".to_string(),
                json!("Reduce selectors, context_radius, before or after and retry."),
            );
            return too_large;
        }
        result
    }

    /// Collect stable references recursively (oracle `evidence_refs`).
    pub fn evidence_refs(value: &serde_json::Value) -> BTreeSet<String> {
        let mut refs = BTreeSet::new();
        match value {
            serde_json::Value::Object(map) => {
                if let Some(reference) = map.get("ref").and_then(|v| v.as_str()) {
                    refs.insert(reference.to_string());
                }
                for child in map.values() {
                    refs.extend(Self::evidence_refs(child));
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    refs.extend(Self::evidence_refs(child));
                }
            }
            _ => {}
        }
        refs
    }
}
