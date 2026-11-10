//! Model agents: translator, analyzer, synopsizer, polisher, extractor, titles.
//!
//! Oracle: `agents/`. Agents render frozen prompt plans and parse model
//! output; the pipeline owns persistence, ordering, and usage accounting.
//! `ModelClient` abstracts `RoutedClient` (async) and `FakeClient` (sync)
//! so golden tests drive the same code without network.

use std::collections::BTreeMap;

use crate::config::Config;
use crate::glossary::GlossaryTerm;
use crate::i18n::policy::PolicyPlan;
use crate::llm::compat::Message;
use crate::llm::json::{parse_json_loose, parse_json_result};
use crate::{Error, Result};

/// Synchronous model completion (fake path drives agents directly).
pub trait ModelClient {
    /// Complete one operation, returning raw model text.
    fn complete(
        &mut self,
        messages: &[Message],
        operation: &str,
        json_mode: bool,
        max_tokens: Option<i64>,
    ) -> Result<String>;
    /// Recorded call count (test introspection; 0 when untracked).
    fn call_count(&self) -> usize {
        0
    }
}

impl ModelClient for crate::llm::fake::FakeClient {
    fn complete(
        &mut self,
        messages: &[Message],
        operation: &str,
        json_mode: bool,
        max_tokens: Option<i64>,
    ) -> Result<String> {
        crate::llm::fake::FakeClient::complete(self, messages, operation, json_mode, max_tokens)
    }

    fn call_count(&self) -> usize {
        self.calls.len()
    }
}

/// Shared agent state: resolved languages + frozen prompt plans.
pub struct Agent {
    /// Source language code.
    pub src: String,
    /// Target language code.
    pub tgt: String,
    /// Frozen plan for this agent's phase.
    pub plan: PolicyPlan,
    /// Alignment retries for malformed model output.
    pub align_retry_limit: i64,
}

impl Agent {
    /// Bind resolved languages and a frozen plan (oracle `Agent.__init__`).
    pub fn new(src: &str, tgt: &str, plan: PolicyPlan, config: &Config) -> Self {
        Self {
            src: src.to_string(),
            tgt: tgt.to_string(),
            plan,
            align_retry_limit: config.pipeline.align_retry_limit,
        }
    }

    /// Render one template with prompt values + explicit arguments.
    pub fn render(&self, name: &str, extra: &BTreeMap<String, String>) -> Result<String> {
        let mut args = extra.clone();
        args.entry("src".to_string())
            .or_insert_with(|| self.src.clone());
        args.entry("tgt".to_string())
            .or_insert_with(|| self.tgt.clone());
        self.plan.render(name, &args)
    }

    /// Complete and parse JSON, returning `(value, raw)` (oracle
    /// `_complete_json_turn`).
    pub fn complete_json_turn(
        &self,
        client: &mut dyn ModelClient,
        messages: &[Message],
        operation: &str,
        max_tokens: Option<i64>,
    ) -> Result<(serde_json::Value, String)> {
        let text = client.complete(messages, operation, true, max_tokens)?;
        let value = parse_json_loose(&text)
            .map_err(|e| Error::Config(format!("Cannot parse the model JSON: {}", e.preview)))?;
        Ok((value, text))
    }

    /// Complete plain text, stripped (oracle `_ask_text`, empty on failure).
    pub fn ask_text(
        &self,
        client: &mut dyn ModelClient,
        system: &str,
        user: &str,
        operation: &str,
    ) -> String {
        let messages = [
            Message {
                role: "system".to_string(),
                content: system.to_string(),
            },
            Message {
                role: "user".to_string(),
                content: user.to_string(),
            },
        ];
        client
            .complete(&messages, operation, false, None)
            .map(|text| text.trim().to_string())
            .unwrap_or_default()
    }

    /// Keep JSON objects, discarding anything else (oracle `dict_items`).
    pub fn dict_items(
        value: &serde_json::Value,
    ) -> Vec<&serde_json::Map<String, serde_json::Value>> {
        match value {
            serde_json::Value::Array(items) => {
                items.iter().filter_map(|item| item.as_object()).collect()
            }
            _ => Vec::new(),
        }
    }
}

/// Normalize a model field to text (oracle `_text`).
pub fn model_text(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(text)) => text.trim().to_string(),
        Some(serde_json::Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

/// Render a numbered list (oracle `prompts.numbered`).
pub fn numbered(texts: &[String]) -> String {
    texts
        .iter()
        .enumerate()
        .map(|(index, text)| format!("[{index}] {text}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render glossary references (oracle `prompts.render_glossary`).
pub fn render_glossary(terms: &[GlossaryTerm], source_lang: &str) -> String {
    if terms.is_empty() {
        return "(none)".to_string();
    }
    terms
        .iter()
        .map(|term| {
            let mut extra = Vec::new();
            if !term.gender.is_empty() {
                extra.push(term.gender.clone());
            }
            if source_lang == "ja" && !term.reading.is_empty() {
                extra.push(format!("Pronunciation: {}", term.reading));
            }
            let tag = if extra.is_empty() {
                format!("({})", term.term_type)
            } else {
                format!("({}, {})", term.term_type, extra.join(", "))
            };
            let alias = if term.aliases.is_empty() {
                String::new()
            } else {
                format!(" [Aliases:  {}]", term.aliases.join(", "))
            };
            format!("- {} → {}{}{}", term.source, term.target, tag, alias)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Quote one following source segment (oracle `render_source_reference`).
pub fn render_source_reference(source: &str) -> String {
    if source.trim().is_empty() {
        "(none)".to_string()
    } else {
        serde_json::to_string(source).unwrap_or_else(|_| "\"\"".to_string())
    }
}

/// One annotation reference for a paragraph.
#[derive(Debug, Clone)]
pub struct AnnotationItem {
    /// Stable target key.
    pub target_key: String,
    /// Source text of the annotation.
    pub source: String,
}

/// Deduplicate annotation data into stable JSON (oracle
/// `render_annotation_contexts`).
pub fn render_annotation_contexts(contexts: &[Vec<AnnotationItem>]) -> Result<String> {
    let mut by_key: BTreeMap<String, (String, Vec<usize>)> = BTreeMap::new();
    for (segment_index, items) in contexts.iter().enumerate() {
        for item in items {
            match by_key.get_mut(&item.target_key) {
                None => {
                    by_key.insert(
                        item.target_key.clone(),
                        (item.source.clone(), vec![segment_index]),
                    );
                }
                Some((source, applies_to)) => {
                    if *source != item.source {
                        return Err(Error::Config(format!(
                            "Inconsistent text for annotation target: {}",
                            item.target_key
                        )));
                    }
                    if !applies_to.contains(&segment_index) {
                        applies_to.push(segment_index);
                    }
                }
            }
        }
    }
    let rendered: Vec<serde_json::Value> = by_key
        .iter()
        .map(|(key, (source, applies_to))| {
            serde_json::json!({
                "target_key": key,
                "source": source,
                "applies_to": applies_to,
            })
        })
        .collect();
    Ok(serde_json::to_string_pretty(&rendered).unwrap_or_else(|_| "[]".to_string()))
}

/// Model output failed alignment validation; the caller retries or falls back.
#[derive(Debug)]
pub struct AlignmentError {
    /// Human-readable cause.
    pub message: String,
}

impl std::fmt::Display for AlignmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AlignmentError {}

/// Body translator (oracle `agents/translator.py`).
pub struct Translator {
    /// Shared agent state.
    pub agent: Agent,
    /// Transcript of the last successful single-shot batch, for polish
    /// continuation.
    pub last_batch_turn: Option<Vec<Message>>,
    /// Source indices translated in that batch.
    pub last_batch_indices: Option<Vec<usize>>,
}

impl Translator {
    /// Bind languages + plan.
    pub fn new(src: &str, tgt: &str, plan: PolicyPlan, config: &Config) -> Self {
        Self {
            agent: Agent::new(src, tgt, plan, config),
            last_batch_turn: None,
            last_batch_indices: None,
        }
    }

    /// Whether a paragraph needs model translation (oracle
    /// `_needs_translation`).
    pub fn needs_translation(source: &str) -> bool {
        let stripped = source.trim();
        !stripped.is_empty() && stripped.chars().any(|c| c.is_alphabetic())
    }

    /// Translate one batch (oracle `_call_batch`).
    #[allow(clippy::too_many_arguments)]
    pub fn call_batch(
        &self,
        client: &mut dyn ModelClient,
        sources: &[String],
        terms: &[GlossaryTerm],
        style: &str,
        context: &str,
        book_synopsis: &str,
        chapter_digest: &str,
        annotation_contexts: &[Vec<AnnotationItem>],
        next_source: &str,
        allow_empty_translations: bool,
    ) -> std::result::Result<(Vec<String>, Vec<Message>), AlignmentError> {
        let fail = |message: &str| AlignmentError {
            message: message.to_string(),
        };
        let n = sources.len();
        let mut extra = BTreeMap::new();
        let style = if style.is_empty() { "(none)" } else { style };
        let book_synopsis = if book_synopsis.is_empty() {
            "(none)"
        } else {
            book_synopsis
        };
        let chapter_digest = if chapter_digest.is_empty() {
            "(none)"
        } else {
            chapter_digest
        };
        let context = if context.is_empty() {
            "(none)"
        } else {
            context
        };
        extra.insert("style".to_string(), style.to_string());
        extra.insert("book_synopsis".to_string(), book_synopsis.to_string());
        extra.insert(
            "glossary".to_string(),
            render_glossary(terms, &self.agent.src),
        );
        let annotation_text =
            render_annotation_contexts(annotation_contexts).map_err(|e| fail(&e.to_string()))?;
        extra.insert("annotation_contexts".to_string(), annotation_text);
        extra.insert("chapter_digest".to_string(), chapter_digest.to_string());
        extra.insert("context".to_string(), context.to_string());
        extra.insert("n".to_string(), n.to_string());
        extra.insert("n_minus_1".to_string(), n.saturating_sub(1).to_string());
        extra.insert("numbered_source".to_string(), numbered(sources));
        extra.insert(
            "next_source".to_string(),
            render_source_reference(next_source),
        );
        let system = self
            .agent
            .render("translator_system", &BTreeMap::new())
            .map_err(|e| fail(&e.to_string()))?;
        let user = self
            .agent
            .render("translator_user", &extra)
            .map_err(|e| fail(&e.to_string()))?;
        let messages = vec![
            Message {
                role: "system".to_string(),
                content: system,
            },
            Message {
                role: "user".to_string(),
                content: user,
            },
        ];
        let (data, raw) = self
            .agent
            .complete_json_turn(client, &messages, "translation.body", None)
            .map_err(|_| fail("Cannot parse the translation JSON returned by the model"))?;
        let repaired = parse_json_result(&raw)
            .map_err(|_| fail("Cannot parse the translation JSON returned by the model"))?;
        if !repaired.safe_for_complete_payload() {
            return Err(fail(
                "The translation JSON required structural repair; retrying",
            ));
        }
        let items = match &data {
            serde_json::Value::Object(map) => map.get("translations").cloned(),
            other => Some(other.clone()),
        };
        let Some(serde_json::Value::Array(items)) = items else {
            return Err(fail("The model did not return a translation array"));
        };
        if items.len() != n {
            return Err(fail(&format!(
                "Translation count mismatch: expected {n} paragraphs, got {}",
                items.len()
            )));
        }
        let mut out = Vec::with_capacity(n);
        for item in &items {
            let Some(text) = item.as_str() else {
                return Err(fail("The model returned a non-string translation"));
            };
            if !allow_empty_translations && text.trim().is_empty() {
                return Err(fail(
                    "The model returned an empty or non-string translation",
                ));
            }
            out.push(text.to_string());
        }
        let turn = vec![
            messages[0].clone(),
            messages[1].clone(),
            Message {
                role: "assistant".to_string(),
                content: raw,
            },
        ];
        Ok((out, turn))
    }

    /// Translate aligned paragraphs (oracle `translate_batch`).
    #[allow(clippy::too_many_arguments)]
    pub fn translate_batch(
        &mut self,
        client: &mut dyn ModelClient,
        sources: &[String],
        terms: &[GlossaryTerm],
        style: &str,
        context: &str,
        book_synopsis: &str,
        chapter_digest: &str,
        annotation_contexts: &[Vec<AnnotationItem>],
        next_source: &str,
        allow_empty_translations: bool,
    ) -> Result<Vec<String>> {
        self.last_batch_turn = None;
        self.last_batch_indices = None;
        let n = sources.len();
        if annotation_contexts.len() != n {
            return Err(Error::Config(format!(
                "Annotation context count mismatch: expected {n} groups, got {}",
                annotation_contexts.len()
            )));
        }
        if n == 0 {
            return Ok(Vec::new());
        }
        let translated_indices: Vec<usize> = sources
            .iter()
            .enumerate()
            .filter(|(_, source)| Self::needs_translation(source))
            .map(|(index, _)| index)
            .collect();
        if translated_indices.is_empty() {
            return Ok(sources.to_vec());
        }
        let translated_sources: Vec<String> = translated_indices
            .iter()
            .map(|&index| sources[index].clone())
            .collect();
        let translated_annotations: Vec<Vec<AnnotationItem>> = translated_indices
            .iter()
            .map(|&index| annotation_contexts[index].clone())
            .collect();
        let following = translated_indices[translated_indices.len() - 1] + 1;
        let batch_next_source: &str = if following < n {
            &sources[following]
        } else {
            next_source
        };
        let attempts = self.agent.align_retry_limit.max(0) as usize + 1;
        for _ in 0..attempts {
            match self.call_batch(
                client,
                &translated_sources,
                terms,
                style,
                context,
                book_synopsis,
                chapter_digest,
                &translated_annotations,
                batch_next_source,
                allow_empty_translations,
            ) {
                Ok((translated, turn)) => {
                    let mut targets = sources.to_vec();
                    for (position, target) in translated_indices.iter().zip(translated) {
                        targets[*position] = target;
                    }
                    self.last_batch_turn = Some(turn);
                    self.last_batch_indices = Some(translated_indices.clone());
                    return Ok(targets);
                }
                Err(_) => continue,
            }
        }
        // Final fallback: one paragraph per call. Any failure stops the
        // chapter explicitly; saved batches remain resumable.
        self.last_batch_turn = None;
        self.last_batch_indices = None;
        let mut targets = sources.to_vec();
        for (position, index) in translated_indices.iter().enumerate() {
            let _ = position;
            let neighbor = if *index + 1 < n {
                sources[*index + 1].clone()
            } else {
                next_source.to_string()
            };
            match self.call_batch(
                client,
                &[sources[*index].clone()],
                terms,
                style,
                context,
                book_synopsis,
                chapter_digest,
                &[translated_annotations[position].clone()],
                &neighbor,
                allow_empty_translations,
            ) {
                Ok((translated, _)) => targets[*index] = translated[0].clone(),
                Err(error) => {
                    return Err(Error::Config(format!(
                        "Single-paragraph fallback failed at paragraph {index}: {}",
                        error.message
                    )))
                }
            }
        }
        Ok(targets)
    }
}

/// Style analyzer (oracle `agents/analyzer.py`).
pub struct Analyzer {
    /// Shared agent state (analysis phase).
    pub agent: Agent,
}

impl Analyzer {
    /// Bind languages + analysis plan.
    pub fn new(src: &str, tgt: &str, plan: PolicyPlan, config: &Config) -> Self {
        Self {
            agent: Agent::new(src, tgt, plan, config),
        }
    }

    /// Analyze samples into style/character/terminology data.
    pub fn analyze(
        &self,
        client: &mut dyn ModelClient,
        sample: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>> {
        let system = self.agent.render("analyzer_system", &BTreeMap::new())?;
        let mut extra = BTreeMap::new();
        extra.insert("sample".to_string(), sample.to_string());
        let user = self.agent.render("analyzer_user", &extra)?;
        let messages = vec![
            Message {
                role: "system".to_string(),
                content: system,
            },
            Message {
                role: "user".to_string(),
                content: user,
            },
        ];
        let (data, _) = self
            .agent
            .complete_json_turn(client, &messages, "analysis.style", None)?;
        let mut data = data.as_object().cloned().unwrap_or_default();
        if let Some(serde_json::Value::Array(bullets)) = data.get("style_guide") {
            let joined = bullets
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            data.insert("style_guide".to_string(), serde_json::Value::String(joined));
        }
        for key in [
            "genre",
            "tone",
            "style_guide",
            "narration",
            "pacing",
            "register",
            "dialogue_style",
            "rhetoric",
        ] {
            let text = model_text(data.get(key));
            data.insert(key.to_string(), serde_json::Value::String(text));
        }
        let characters = data
            .get("characters")
            .map(Agent::dict_items)
            .unwrap_or_default()
            .iter()
            .map(|item| {
                let mut item = (*item).clone();
                let gender = crate::glossary::store::normalize_gender(
                    model_text(item.get("gender")).as_str(),
                );
                item.insert("gender".to_string(), serde_json::Value::String(gender));
                if self.agent.src != "ja" {
                    item.remove("reading");
                }
                serde_json::Value::Object(item)
            })
            .collect::<Vec<_>>();
        data.insert(
            "characters".to_string(),
            serde_json::Value::Array(characters),
        );
        let terms = data
            .get("terms")
            .map(Agent::dict_items)
            .unwrap_or_default()
            .iter()
            .map(|item| {
                let mut item = (*item).clone();
                let term_type = crate::glossary::store::normalize_term_type(
                    model_text(item.get("type")).as_str(),
                );
                item.insert("type".to_string(), serde_json::Value::String(term_type));
                if self.agent.src != "ja" {
                    item.remove("reading");
                }
                serde_json::Value::Object(item)
            })
            .collect::<Vec<_>>();
        data.insert("terms".to_string(), serde_json::Value::Array(terms));
        Ok(data)
    }

    /// Seed analyzed characters + terms (oracle `seed_glossary`).
    pub fn seed_terms(
        &self,
        analysis: &serde_json::Map<String, serde_json::Value>,
    ) -> Vec<GlossaryTerm> {
        let mut out = Vec::new();
        let empty = Vec::new();
        let characters = analysis
            .get("characters")
            .map(Agent::dict_items)
            .unwrap_or(empty);
        for item in characters {
            let source = model_text(item.get("source"));
            let target = model_text(item.get("target"));
            if source.is_empty() || target.is_empty() {
                continue;
            }
            out.push(GlossaryTerm {
                source,
                target,
                reading: if self.agent.src == "ja" {
                    model_text(item.get("reading"))
                } else {
                    String::new()
                },
                term_type: "person".to_string(),
                gender: model_text(item.get("gender")),
                aliases: Vec::new(),
                first_chapter: Some(0),
                note: model_text(item.get("note")),
                status: "ok".to_string(),
                mentions: 0,
            });
        }
        let empty = Vec::new();
        let terms = analysis
            .get("terms")
            .map(Agent::dict_items)
            .unwrap_or(empty);
        for item in terms {
            let source = model_text(item.get("source"));
            let target = model_text(item.get("target"));
            if source.is_empty() || target.is_empty() {
                continue;
            }
            out.push(GlossaryTerm {
                source,
                target,
                reading: if self.agent.src == "ja" {
                    model_text(item.get("reading"))
                } else {
                    String::new()
                },
                term_type: crate::glossary::store::normalize_term_type(
                    model_text(item.get("type")).as_str(),
                ),
                gender: String::new(),
                aliases: Vec::new(),
                first_chapter: Some(0),
                note: model_text(item.get("note")),
                status: "ok".to_string(),
                mentions: 0,
            });
        }
        out
    }

    /// Condense style + character guidance (oracle `style_brief`).
    pub fn style_brief(analysis: &serde_json::Map<String, serde_json::Value>) -> String {
        let mut lines = Vec::new();
        let get = |key: &str| model_text(analysis.get(key));
        if !get("genre").is_empty() {
            lines.push(format!("Genre: {}", get("genre")));
        }
        if !get("tone").is_empty() {
            lines.push(format!("Tone: {}", get("tone")));
        }
        if !get("style_guide").is_empty() {
            lines.push(format!("Style guide: {}", get("style_guide")));
        }
        for (key, tag) in [
            ("narration", "Narration"),
            ("pacing", "Pacing"),
            ("register", "Register"),
            ("dialogue_style", "Dialogue style"),
            ("rhetoric", "Rhetoric"),
        ] {
            if !get(key).is_empty() {
                lines.push(format!("{tag}: {}", get(key)));
            }
        }
        let empty = serde_json::Value::Array(Vec::new());
        let chars = Agent::dict_items(analysis.get("characters").unwrap_or(&empty));
        if !chars.is_empty() {
            lines.push("Characters: ".to_string());
            for item in chars {
                let gender = crate::glossary::store::normalize_gender(
                    model_text(item.get("gender")).as_str(),
                );
                let suffix = if gender.is_empty() {
                    String::new()
                } else {
                    format!(", {gender}")
                };
                let note = model_text(item.get("note"));
                let note = if note.is_empty() {
                    String::new()
                } else {
                    format!(", {note}")
                };
                lines.push(format!(
                    "  - {}{}{}",
                    model_text(item.get("source")),
                    suffix,
                    note
                ));
            }
        }
        lines.join("\n")
    }
}

/// Prescan synopsizer (oracle `agents/synopsis.py`).
pub struct Synopsizer {
    /// Shared agent state (analysis phase).
    pub agent: Agent,
}

/// Character budget for one digest merge.
pub const REDUCE_BUDGET: usize = 12000;
/// Source characters covered by one digest call.
pub const DIGEST_SOURCE_BUDGET: usize = 8000;

impl Synopsizer {
    /// Bind languages + analysis plan.
    pub fn new(src: &str, tgt: &str, plan: PolicyPlan, config: &Config) -> Self {
        Self {
            agent: Agent::new(src, tgt, plan, config),
        }
    }

    /// Summarize one chapter (oracle `digest_chapter`).
    pub fn digest_chapter(&self, client: &mut dyn ModelClient, source_text: &str) -> String {
        if source_text.trim().is_empty() {
            return String::new();
        }
        let Ok(system) = self.agent.render("chapter_digest_system", &BTreeMap::new()) else {
            return String::new();
        };
        let mut extra = BTreeMap::new();
        extra.insert("source".to_string(), Self::digest_source(source_text));
        let Ok(user) = self.agent.render("chapter_digest_user", &extra) else {
            return String::new();
        };
        self.agent
            .ask_text(client, &system, &user, "synopsis.chapter")
    }

    /// Bound digest input with head + tail + omission marker.
    pub fn digest_source(source_text: &str) -> String {
        let chars = source_text.chars().count();
        if chars <= DIGEST_SOURCE_BUDGET {
            return source_text.to_string();
        }
        let head = DIGEST_SOURCE_BUDGET * 3 / 4;
        let tail = DIGEST_SOURCE_BUDGET - head;
        let omitted = chars - DIGEST_SOURCE_BUDGET;
        let head_text: String = source_text.chars().take(head).collect();
        let tail_text: String = source_text.chars().skip(chars - tail).collect();
        format!("{head_text}\n\n[… {omitted} characters omitted …]\n\n{tail_text}")
    }

    /// Combine digests into a book synopsis with map-reduce.
    pub fn book_synopsis(
        &self,
        client: &mut dyn ModelClient,
        digests: &[String],
        analysis_brief: &str,
    ) -> String {
        let mut items: Vec<String> = digests
            .iter()
            .map(|digest| digest.trim().to_string())
            .filter(|digest| !digest.is_empty())
            .collect();
        if items.is_empty() {
            return String::new();
        }
        loop {
            let groups = Self::group(&items, REDUCE_BUDGET);
            if groups.len() == 1 {
                return self.synth(client, &groups[0], analysis_brief);
            }
            let mut summaries = Vec::new();
            for group in &groups {
                let summary = self.synth(client, group, analysis_brief);
                if summary.is_empty() {
                    return String::new();
                }
                summaries.push(summary);
            }
            items = summaries;
        }
    }

    /// Greedily group strings by character budget.
    pub fn group(items: &[String], budget: usize) -> Vec<Vec<String>> {
        let mut groups: Vec<Vec<String>> = Vec::new();
        let mut current: Vec<String> = Vec::new();
        let mut size = 0;
        for item in items {
            if !current.is_empty() && size + item.chars().count() > budget {
                groups.push(std::mem::take(&mut current));
                size = 0;
            }
            current.push(item.clone());
            size += item.chars().count() + 1;
        }
        if !current.is_empty() {
            groups.push(current);
        }
        groups
    }

    /// Merge one group of digests into a higher-level synopsis.
    fn synth(
        &self,
        client: &mut dyn ModelClient,
        digests: &[String],
        analysis_brief: &str,
    ) -> String {
        let numbered_digests = numbered(digests);
        let Ok(system) = self.agent.render("book_synopsis_system", &BTreeMap::new()) else {
            return String::new();
        };
        let mut extra = BTreeMap::new();
        extra.insert(
            "analysis".to_string(),
            if analysis_brief.is_empty() {
                "(none)".to_string()
            } else {
                analysis_brief.to_string()
            },
        );
        extra.insert("digests".to_string(), numbered_digests);
        let Ok(user) = self.agent.render("book_synopsis_user", &extra) else {
            return String::new();
        };
        self.agent.ask_text(client, &system, &user, "synopsis.book")
    }
}

/// Polishing agent (oracle `agents/polisher.py`).
pub struct Polisher {
    /// Shared agent state.
    pub agent: Agent,
    /// Whether the last standalone polish failed alignment.
    pub last_polish_failed: bool,
}

impl Polisher {
    /// Bind languages + plan.
    pub fn new(src: &str, tgt: &str, plan: PolicyPlan, config: &Config) -> Self {
        Self {
            agent: Agent::new(src, tgt, plan, config),
            last_polish_failed: false,
        }
    }

    /// Polish an aligned list in a fresh conversation.
    pub fn polish(
        &mut self,
        client: &mut dyn ModelClient,
        targets: &[String],
        terms: &[GlossaryTerm],
        style: &str,
        next_source: &str,
    ) -> Vec<String> {
        if targets.is_empty() {
            return Vec::new();
        }
        let n = targets.len();
        let mut extra = BTreeMap::new();
        extra.insert("n".to_string(), n.to_string());
        let Ok(system) = self.agent.render("polisher_system", &extra) else {
            self.last_polish_failed = true;
            return targets.to_vec();
        };
        let mut extra = BTreeMap::new();
        extra.insert(
            "glossary".to_string(),
            render_glossary(terms, &self.agent.src),
        );
        extra.insert(
            "style".to_string(),
            if style.is_empty() {
                "(none)".to_string()
            } else {
                style.to_string()
            },
        );
        extra.insert("n".to_string(), n.to_string());
        extra.insert("numbered_target".to_string(), numbered(targets));
        extra.insert(
            "next_source".to_string(),
            render_source_reference(next_source),
        );
        let Ok(user) = self.agent.render("polisher_user", &extra) else {
            self.last_polish_failed = true;
            return targets.to_vec();
        };
        let messages = vec![
            Message {
                role: "system".to_string(),
                content: system,
            },
            Message {
                role: "user".to_string(),
                content: user,
            },
        ];
        let items = self
            .agent
            .complete_json_turn(client, &messages, "polish.body", None)
            .ok()
            .and_then(|(data, _)| match data {
                serde_json::Value::Object(map) => map.get("polished").cloned(),
                other => Some(other),
            });
        match items {
            Some(serde_json::Value::Array(items)) if items.len() == n => {
                self.last_polish_failed = false;
                items
                    .iter()
                    .map(|item| match item {
                        serde_json::Value::String(text) => text.clone(),
                        other => other.to_string(),
                    })
                    .collect()
            }
            _ => {
                self.last_polish_failed = true;
                targets.to_vec()
            }
        }
    }

    /// Append a polish turn to a translation transcript.
    pub fn polish_continue(
        &self,
        client: &mut dyn ModelClient,
        turn: &[Message],
        n: usize,
        next_source: &str,
    ) -> Option<Vec<String>> {
        if n == 0 || turn.len() < 3 {
            return None;
        }
        let mut extra = BTreeMap::new();
        extra.insert("n".to_string(), n.to_string());
        extra.insert(
            "next_source".to_string(),
            render_source_reference(next_source),
        );
        let continue_user = self.agent.render("polisher_continue_user", &extra).ok()?;
        let mut messages = turn.to_vec();
        messages.push(Message {
            role: "user".to_string(),
            content: continue_user,
        });
        let (data, _) = self
            .agent
            .complete_json_turn(client, &messages, "polish.body", None)
            .ok()?;
        let items = match data {
            serde_json::Value::Object(map) => map.get("polished").cloned(),
            other => Some(other),
        };
        match items {
            Some(serde_json::Value::Array(items)) if items.len() == n => Some(
                items
                    .iter()
                    .map(|item| match item {
                        serde_json::Value::String(text) => text.clone(),
                        other => other.to_string(),
                    })
                    .collect(),
            ),
            _ => None,
        }
    }
}

/// Title translator (oracle `agents/title_translator.py`).
pub struct TitleTranslator {
    /// Shared agent state.
    pub agent: Agent,
}

/// Title response carried the wrong item count.
#[derive(Debug)]
pub struct TitleOutputError {
    /// Expected item count.
    pub expected: usize,
    /// Actual item count, if a list was returned.
    pub actual: Option<usize>,
}

impl std::fmt::Display for TitleOutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Chapter/TOC title translation returned an invalid number of items: expected {}, got {}",
            self.expected,
            self.actual
                .map(|n| n.to_string())
                .unwrap_or_else(|| "non-list".to_string())
        )
    }
}

impl std::error::Error for TitleOutputError {}

impl TitleTranslator {
    /// Bind languages + plan.
    pub fn new(src: &str, tgt: &str, plan: PolicyPlan, config: &Config) -> Self {
        Self {
            agent: Agent::new(src, tgt, plan, config),
        }
    }

    /// Translate aligned titles.
    pub fn translate(
        &self,
        client: &mut dyn ModelClient,
        titles: &[String],
        glossary_text: &str,
    ) -> std::result::Result<Vec<String>, TitleOutputError> {
        let n = titles.len();
        let mut extra = BTreeMap::new();
        extra.insert("n".to_string(), n.to_string());
        let system = self
            .agent
            .render("title_translator_system", &extra)
            .map_err(|_| TitleOutputError {
                expected: n,
                actual: None,
            })?;
        let mut extra = BTreeMap::new();
        extra.insert("glossary".to_string(), glossary_text.to_string());
        extra.insert("n".to_string(), n.to_string());
        extra.insert("numbered_titles".to_string(), numbered(titles));
        let user = self
            .agent
            .render("title_translator_user", &extra)
            .map_err(|_| TitleOutputError {
                expected: n,
                actual: None,
            })?;
        let messages = vec![
            Message {
                role: "system".to_string(),
                content: system,
            },
            Message {
                role: "user".to_string(),
                content: user,
            },
        ];
        let (data, _) = self
            .agent
            .complete_json_turn(client, &messages, "translation.title", None)
            .map_err(|_| TitleOutputError {
                expected: n,
                actual: None,
            })?;
        let out = match &data {
            serde_json::Value::Object(map) => map.get("titles").cloned(),
            other => Some(other.clone()),
        };
        match out {
            Some(serde_json::Value::Array(items)) if items.len() == n => Ok(items
                .iter()
                .map(|item| match item {
                    serde_json::Value::String(text) => text.trim().to_string(),
                    other => other.to_string().trim().to_string(),
                })
                .collect()),
            Some(serde_json::Value::Array(items)) => Err(TitleOutputError {
                expected: n,
                actual: Some(items.len()),
            }),
            _ => Err(TitleOutputError {
                expected: n,
                actual: None,
            }),
        }
    }
}

/// One translated paragraph with its book position.
#[derive(Debug, Clone)]
pub struct TranslatedEvidence {
    /// Chapter ordinal.
    pub chapter: i64,
    /// Segment ordinal within the chapter.
    pub segment: i64,
    /// Source text.
    pub source: String,
    /// Translated text.
    pub target: String,
}

/// Glossary extractor (oracle `glossary/extractor.py`).
pub struct GlossaryExtractor {
    /// Shared agent state.
    pub agent: Agent,
    /// Cached sorted history for first-occurrence scans.
    occurrence_history: Option<Vec<TranslatedEvidence>>,
    /// Cached per-term first hits.
    occurrence_index: BTreeMap<String, TranslatedEvidence>,
}

impl GlossaryExtractor {
    /// Bind languages + plan.
    pub fn new(src: &str, tgt: &str, plan: PolicyPlan, config: &Config) -> Self {
        Self {
            agent: Agent::new(src, tgt, plan, config),
            occurrence_history: None,
            occurrence_index: BTreeMap::new(),
        }
    }

    /// Extract terms from one source/target pair.
    pub fn extract(
        &self,
        client: &mut dyn ModelClient,
        source_text: &str,
        target_text: &str,
        existing: &[GlossaryTerm],
    ) -> Vec<GlossaryTerm> {
        let Ok(system) = self
            .agent
            .render("glossary_extractor_system", &BTreeMap::new())
        else {
            return Vec::new();
        };
        let mut extra = BTreeMap::new();
        extra.insert(
            "glossary".to_string(),
            render_glossary(existing, &self.agent.src),
        );
        extra.insert("source".to_string(), source_text.to_string());
        extra.insert("target".to_string(), target_text.to_string());
        let Ok(user) = self.agent.render("glossary_extractor_user", &extra) else {
            return Vec::new();
        };
        let messages = vec![
            Message {
                role: "system".to_string(),
                content: system,
            },
            Message {
                role: "user".to_string(),
                content: user,
            },
        ];
        let Ok((data, _)) =
            self.agent
                .complete_json_turn(client, &messages, "glossary.extract", None)
        else {
            return Vec::new();
        };
        let empty = serde_json::Value::Array(Vec::new());
        let raw = match &data {
            serde_json::Value::Object(map) => map.get("terms").unwrap_or(&empty),
            other => other,
        };
        let mut out = Vec::new();
        for item in Agent::dict_items(raw) {
            let source = model_text(item.get("source"));
            let target = model_text(item.get("target"));
            if source.is_empty() || target.is_empty() {
                continue;
            }
            let aliases = match item.get("aliases") {
                Some(serde_json::Value::Array(items)) => items
                    .iter()
                    .filter_map(|alias| alias.as_str())
                    .map(|alias| alias.trim().to_string())
                    .filter(|alias| !alias.is_empty())
                    .collect(),
                _ => Vec::new(),
            };
            out.push(GlossaryTerm {
                source,
                target,
                reading: if self.agent.src == "ja" {
                    model_text(item.get("reading"))
                } else {
                    String::new()
                },
                term_type: crate::glossary::store::normalize_term_type(
                    model_text(item.get("type")).as_str(),
                ),
                gender: model_text(item.get("gender")),
                aliases,
                first_chapter: None,
                note: model_text(item.get("note")),
                status: "ok".to_string(),
                mentions: 0,
            });
        }
        out
    }

    /// Existing terms occurring at least twice in the corpus (oracle
    /// `_recurring_existing_terms`).
    pub fn recurring_existing_terms(
        terms: &[GlossaryTerm],
        source_corpus: &str,
    ) -> Vec<GlossaryTerm> {
        use crate::glossary::matcher::{match_text, normalize_match_key, TermMatcher};
        let normalized = match_text(source_corpus);
        terms
            .iter()
            .filter(|term| {
                let keys = crate::glossary::store::term_match_sources(term);
                let mut spans: std::collections::BTreeSet<(usize, usize)> = Default::default();
                for key in keys {
                    let normalized_key = normalize_match_key(&key);
                    if normalized_key.is_empty() {
                        continue;
                    }
                    // Count non-overlapping occurrences with boundary checks.
                    let matcher = TermMatcher::new(&[key]);
                    let mut start = 0;
                    while start <= normalized.len() {
                        let Some(position) = normalized[start..].find(&normalized_key) else {
                            break;
                        };
                        let at = start + position;
                        let end = at + normalized_key.len();
                        // Boundary check via full-text matcher on the slice context.
                        let _ = &matcher;
                        if crate::glossary::matcher::matches_normalized_at(
                            &normalized_key,
                            &normalized,
                            at,
                            end,
                        ) {
                            spans.insert((at, end));
                        }
                        start = end.max(start + 1);
                    }
                }
                // Merge overlapping spans into single mentions.
                let mut count = 0;
                let mut active_end = 0usize;
                let mut first = true;
                for (start, end) in spans {
                    if first || start >= active_end {
                        count += 1;
                        first = false;
                    }
                    active_end = active_end.max(end);
                }
                count >= 2
            })
            .cloned()
            .collect()
    }

    /// First translated paragraph before a position for unstored terms.
    pub fn first_occurrences(
        &mut self,
        terms: &[GlossaryTerm],
        is_stored: &dyn Fn(&str) -> bool,
        history: &[TranslatedEvidence],
        before: (i64, i64),
    ) -> BTreeMap<String, TranslatedEvidence> {
        use crate::glossary::matcher::TermMatcher;
        let mut first: BTreeMap<String, TranslatedEvidence> = BTreeMap::new();
        let pending: Vec<&str> = terms
            .iter()
            .map(|term| term.source.as_str())
            .filter(|source| !is_stored(source))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if pending.is_empty() {
            return first;
        }
        let needs_rebuild = self
            .occurrence_history
            .as_ref()
            .map(|cached| cached.len() != history.len())
            .unwrap_or(true);
        if needs_rebuild {
            let mut ordered = history.to_vec();
            ordered.sort_by_key(|item| (item.chapter, item.segment));
            self.occurrence_history = Some(ordered);
            self.occurrence_index.clear();
        }
        let ordered = self.occurrence_history.clone().unwrap_or_default();
        let mut remaining: Vec<&str> = Vec::new();
        for source in &pending {
            if let Some(cached) = self.occurrence_index.get(*source) {
                if (cached.chapter, cached.segment) < before {
                    first.insert((*source).to_string(), cached.clone());
                    continue;
                }
            }
            remaining.push(source);
        }
        if remaining.is_empty() {
            return first;
        }
        let matchers: BTreeMap<&str, TermMatcher> = remaining
            .iter()
            .map(|source| (*source, TermMatcher::new(&[(*source).to_string()])))
            .collect();
        for evidence in &ordered {
            if (evidence.chapter, evidence.segment) >= before {
                continue;
            }
            for source in &remaining {
                if first.contains_key(*source) {
                    continue;
                }
                if matchers[*source].matches(&evidence.source) {
                    first.insert((*source).to_string(), evidence.clone());
                    self.occurrence_index
                        .insert((*source).to_string(), evidence.clone());
                }
            }
            if first.len() == pending.len() {
                break;
            }
        }
        first
    }

    /// Align candidates with first translations; defer uncertain mappings.
    pub fn align_with_first_occurrences(
        &self,
        client: &mut dyn ModelClient,
        terms: Vec<GlossaryTerm>,
        occurrences: &BTreeMap<String, TranslatedEvidence>,
    ) -> (Vec<GlossaryTerm>, usize, usize) {
        if occurrences.is_empty() {
            return (terms, 0, 0);
        }
        let candidates: Vec<serde_json::Value> = terms
            .iter()
            .filter_map(|term| {
                occurrences.get(&term.source).map(|evidence| {
                    serde_json::json!({
                        "source": term.source,
                        "proposed_target": term.target,
                        "first_occurrence": {
                            "chapter": evidence.chapter,
                            "segment": evidence.segment,
                            "source": evidence.source,
                            "target": evidence.target,
                        },
                    })
                })
            })
            .collect();
        let Ok(system) = self
            .agent
            .render("glossary_history_system", &BTreeMap::new())
        else {
            return (terms, 0, 0);
        };
        let mut extra = BTreeMap::new();
        extra.insert(
            "candidates_json".to_string(),
            serde_json::to_string_pretty(&candidates).unwrap_or_else(|_| "[]".to_string()),
        );
        let Ok(user) = self.agent.render("glossary_history_user", &extra) else {
            return (terms, 0, 0);
        };
        let messages = vec![
            Message {
                role: "system".to_string(),
                content: system,
            },
            Message {
                role: "user".to_string(),
                content: user,
            },
        ];
        let mut resolved: BTreeMap<String, String> = BTreeMap::new();
        if let Ok((data, _)) =
            self.agent
                .complete_json_turn(client, &messages, "glossary.align_history", None)
        {
            let empty = serde_json::Value::Array(Vec::new());
            let raw = match &data {
                serde_json::Value::Object(map) => map.get("terms").unwrap_or(&empty),
                other => other,
            };
            for item in Agent::dict_items(raw) {
                let source = model_text(item.get("source"));
                let target = model_text(item.get("target"));
                if !source.is_empty() && !target.is_empty() && occurrences.contains_key(&source) {
                    resolved.insert(source, target);
                }
            }
        }
        let aligned_count = resolved.len();
        let mut aligned = Vec::new();
        let mut unresolved = 0;
        for term in terms {
            if !occurrences.contains_key(&term.source) {
                aligned.push(term);
                continue;
            }
            match resolved.get(&term.source) {
                Some(target) => aligned.push(GlossaryTerm {
                    target: target.clone(),
                    ..term
                }),
                None => unresolved += 1,
            }
        }
        (aligned, aligned_count, unresolved)
    }
}
