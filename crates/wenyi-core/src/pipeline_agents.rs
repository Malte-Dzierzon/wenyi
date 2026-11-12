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

/// Render aligned source/target pairs (oracle `prompts.numbered_pairs`).
pub fn numbered_pairs(sources: &[String], targets: &[String]) -> String {
    sources
        .iter()
        .zip(targets.iter())
        .enumerate()
        .map(|(index, (source, target))| {
            format!("[{index}] Source: {source}\n    Translation: {target}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render pairs with stable segment refs (oracle `numbered_pairs_with_refs`).
pub fn numbered_pairs_with_refs(sources: &[String], targets: &[String], refs: &[String]) -> String {
    sources
        .iter()
        .zip(targets.iter())
        .enumerate()
        .map(|(index, (source, target))| {
            let reference = refs.get(index).map(String::as_str).unwrap_or("");
            format!(
                "[{index}] ref={} Source: {source}\n    Translation: {target}",
                if reference.is_empty() {
                    "(none)"
                } else {
                    reference
                }
            )
        })
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

    /// Observe completed chapter evidence against the active profile and
    /// propose refinements (Slice 5; no oracle counterpart).
    ///
    /// Returns `(property, after, reason, confidence, scope)` proposals.
    /// Unparseable model output yields no proposals, never an error.
    pub fn observe(
        &self,
        client: &mut dyn ModelClient,
        profile_version: i64,
        profile: &str,
        evidence: &str,
    ) -> Vec<(String, String, String, f64, String)> {
        let system = match self.agent.render("style_observer_system", &BTreeMap::new()) {
            Ok(text) => text,
            Err(_) => return Vec::new(),
        };
        let mut extra = BTreeMap::new();
        extra.insert("profile_version".to_string(), profile_version.to_string());
        extra.insert("profile".to_string(), profile.to_string());
        extra.insert("evidence".to_string(), evidence.to_string());
        let user = match self.agent.render("style_observer_user", &extra) {
            Ok(text) => text,
            Err(_) => return Vec::new(),
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
        let (data, _) =
            match self
                .agent
                .complete_json_turn(client, &messages, "style.observe", None)
            {
                Ok(pair) => pair,
                Err(_) => return Vec::new(),
            };
        let empty = Vec::new();
        let mut out = Vec::new();
        for item in data
            .get("proposals")
            .map(Agent::dict_items)
            .unwrap_or(empty)
        {
            let property = model_text(item.get("property"));
            let after = model_text(item.get("after"));
            if property.is_empty() || after.is_empty() {
                continue;
            }
            let confidence = item
                .get("confidence")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let scope = model_text(item.get("scope"));
            out.push((
                property,
                after,
                model_text(item.get("reason")),
                confidence,
                if scope == "book" {
                    "book".to_string()
                } else {
                    "series".to_string()
                },
            ));
        }
        out
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

/// Structured review output error retried with smaller input (oracle
/// `ReviewOutputError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewOutputError {
    /// Stable reason code.
    pub reason: String,
}

impl std::fmt::Display for ReviewOutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Review output protocol error: {}", self.reason)
    }
}

impl std::error::Error for ReviewOutputError {}

/// One validated review issue (oracle `Reviewer._validate_issues` output).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewIssue {
    /// Chunk-local segment index.
    pub index: i64,
    /// Issue type (missing/added/mistranslation/terminology/pronoun).
    pub issue_type: String,
    /// Description.
    pub detail: String,
    /// Corrected translation or specific edit.
    pub suggestion: String,
}

/// Cheap-tier paragraph reviewer (oracle `agents/reviewer.py`).
pub struct Reviewer {
    /// Shared agent state (review phase).
    pub agent: Agent,
}

impl Reviewer {
    /// Bind languages + review plan.
    pub fn new(src: &str, tgt: &str, plan: PolicyPlan, config: &Config) -> Self {
        Self {
            agent: Agent::new(src, tgt, plan, config),
        }
    }

    /// Review pairs into validated issues (oracle `Reviewer.review_result`).
    pub fn review_result(
        &self,
        client: &mut dyn ModelClient,
        sources: &[String],
        targets: &[String],
        glossary_terms: &[GlossaryTerm],
    ) -> Result<(Vec<ReviewIssue>, bool), ReviewOutputError> {
        if sources.is_empty() {
            return Ok((Vec::new(), false));
        }
        let mut extra = BTreeMap::new();
        extra.insert("n".to_string(), sources.len().to_string());
        let system =
            self.agent
                .render("reviewer_system", &extra)
                .map_err(|_| ReviewOutputError {
                    reason: "render_failed".to_string(),
                })?;
        let mut extra = BTreeMap::new();
        extra.insert("n".to_string(), sources.len().to_string());
        extra.insert(
            "glossary".to_string(),
            render_glossary(glossary_terms, &self.agent.src),
        );
        extra.insert("pairs".to_string(), numbered_pairs(sources, targets));
        let user = self
            .agent
            .render("reviewer_user", &extra)
            .map_err(|_| ReviewOutputError {
                reason: "render_failed".to_string(),
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
        let text = client
            .complete(&messages, "review.scan", true, None)
            .map_err(|error| ReviewOutputError {
                reason: format!("model_error:{error}"),
            })?;
        let parsed = parse_json_result(&text).map_err(|_| ReviewOutputError {
            reason: "malformed_json".to_string(),
        })?;
        if !parsed.safe_for_complete_payload() {
            return Err(ReviewOutputError {
                reason: "unsafe_json_repair".to_string(),
            });
        }
        let data = parsed.value.as_object().ok_or_else(|| ReviewOutputError {
            reason: "response_not_object".to_string(),
        })?;
        let reviewed = data.get("reviewed_segments");
        let matches = matches!(reviewed, Some(serde_json::Value::Number(n)) if n.as_i64() == Some(sources.len() as i64));
        if !matches {
            return Err(ReviewOutputError {
                reason: "reviewed_segments_mismatch".to_string(),
            });
        }
        if data.get("complete") != Some(&serde_json::Value::Bool(true)) {
            return Err(ReviewOutputError {
                reason: "completion_marker_missing".to_string(),
            });
        }
        let issues = data
            .get("issues")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ReviewOutputError {
                reason: "issues_not_list".to_string(),
            })?;
        if issues.iter().any(|item| !item.is_object()) {
            return Err(ReviewOutputError {
                reason: "issue_not_object".to_string(),
            });
        }
        let validated = Self::validate_issues(issues, sources.len())?;
        Ok((validated, parsed.repaired))
    }

    /// Normalize and validate every candidate (oracle `_validate_issues`).
    pub fn validate_issues(
        issues: &[serde_json::Value],
        segment_count: usize,
    ) -> Result<Vec<ReviewIssue>, ReviewOutputError> {
        let allowed = [
            "missing",
            "added",
            "mistranslation",
            "terminology",
            "pronoun",
        ];
        let mut out = Vec::new();
        for item in issues {
            let obj = item.as_object().ok_or_else(|| ReviewOutputError {
                reason: "issue_not_object".to_string(),
            })?;
            let index = match obj.get("index") {
                Some(serde_json::Value::Number(n)) => n.as_i64(),
                Some(serde_json::Value::String(text)) => text.trim().parse::<i64>().ok(),
                _ => None,
            }
            .ok_or_else(|| ReviewOutputError {
                reason: "invalid_issue_index".to_string(),
            })?;
            if index < 0 || index >= segment_count as i64 {
                return Err(ReviewOutputError {
                    reason: "invalid_issue_index".to_string(),
                });
            }
            let issue_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or_default();
            if !allowed.contains(&issue_type) {
                return Err(ReviewOutputError {
                    reason: "invalid_issue_type".to_string(),
                });
            }
            let detail = obj
                .get("detail")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let suggestion = obj
                .get("suggestion")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if detail.trim().is_empty() {
                return Err(ReviewOutputError {
                    reason: "invalid_issue_detail".to_string(),
                });
            }
            if suggestion.trim().is_empty() {
                return Err(ReviewOutputError {
                    reason: "invalid_issue_suggestion".to_string(),
                });
            }
            out.push(ReviewIssue {
                index,
                issue_type: issue_type.to_string(),
                detail: detail.trim().to_string(),
                suggestion: suggestion.trim().to_string(),
            });
        }
        Ok(out)
    }
}

/// Fixer protocol violation (oracle `ReviewFixerProtocolError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFixerProtocolError {
    /// Stable reason code.
    pub reason: String,
}

impl std::fmt::Display for ReviewFixerProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Review fixer protocol error: {}", self.reason)
    }
}

impl std::error::Error for ReviewFixerProtocolError {}

/// Complete paragraph replacement for the next review (oracle
/// `ProvisionalPatch`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionalPatch {
    /// Content-sensitive patch id.
    pub patch_id: String,
    /// Fix round.
    pub round: i64,
    /// Stable segment reference.
    pub segment_ref: String,
    /// Chapter index.
    pub chapter: i64,
    /// Text index.
    pub index: i64,
    /// Hash of the current translation.
    pub before_hash: String,
    /// Current translation.
    pub before: String,
    /// Replacement translation.
    pub after: String,
    /// Covered issue ids.
    pub issue_ids: Vec<String>,
}

/// Count complete double-quote pairs (oracle `_dialogue_quote_pairs`).
pub fn dialogue_quote_pairs(text: &str) -> usize {
    text.matches('"').count() / 2
        + text.matches('“').count().min(text.matches('”').count())
        + text.matches('「').count().min(text.matches('」').count())
}

/// Derive a reproducible patch id (oracle `_patch_id`).
pub fn fixer_patch_id(
    round_number: i64,
    segment_ref: &str,
    before_hash: &str,
    after: &str,
    issue_ids: &[String],
) -> String {
    let payload = serde_json::json!({
        "round": round_number,
        "segment_ref": segment_ref,
        "before_hash": before_hash,
        "after": after,
        "issue_ids": issue_ids,
    });
    format!(
        "patch-r{round_number:02}-{:.16}",
        crate::review_models::text_hash(&payload.to_string())
    )
}

/// Render neighboring pairs as read-only context (oracle `_nearby_text`).
pub fn fixer_nearby_text(pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return "(none)".to_string();
    }
    pairs
        .iter()
        .enumerate()
        .map(|(ordinal, (source, target))| {
            format!("[Context {ordinal}] Source: {source}\n    Translation: {target}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Single-paragraph patch generator (oracle `agents/review_fixer.py`).
pub struct ReviewFixer {
    /// Shared agent state (review phase).
    pub agent: Agent,
    /// Operation id (`review.fix` or `autofix.fix`).
    pub operation: String,
}

impl ReviewFixer {
    /// Bind languages + plan + operation.
    pub fn new(src: &str, tgt: &str, plan: PolicyPlan, config: &Config, operation: &str) -> Self {
        Self {
            agent: Agent::new(src, tgt, plan, config),
            operation: operation.to_string(),
        }
    }

    /// Hash the current translation (oracle `ReviewFixer.target_hash`).
    pub fn target_hash(target: &str) -> String {
        crate::review_models::text_hash(target)
    }

    /// Generate a temporary replacement (oracle `ReviewFixer.propose`).
    #[allow(clippy::too_many_arguments)]
    pub fn propose(
        &self,
        client: &mut dyn ModelClient,
        round_number: i64,
        segment_ref: &str,
        chapter: i64,
        index: i64,
        source: &str,
        current_target: &str,
        issues: &[serde_json::Map<String, serde_json::Value>],
        style: &str,
        book_synopsis: &str,
        chapter_digest: &str,
        relevant_glossary: &[GlossaryTerm],
        nearby_pairs: &[(String, String)],
    ) -> Result<ProvisionalPatch, ReviewFixerProtocolError> {
        let fail = |reason: &str| ReviewFixerProtocolError {
            reason: reason.to_string(),
        };
        if round_number < 1 {
            return Err(fail("invalid_round"));
        }
        let segment_ref = segment_ref.trim();
        if segment_ref.is_empty() {
            return Err(fail("invalid_segment_ref"));
        }
        if chapter < 0 {
            return Err(fail("invalid_chapter"));
        }
        if index < 0 {
            return Err(fail("invalid_index"));
        }
        if source.trim().is_empty() {
            return Err(fail("empty_source"));
        }
        if current_target.trim().is_empty() {
            return Err(fail("empty_current_target"));
        }
        if issues.is_empty() {
            return Err(fail("issues_required"));
        }
        let mut issue_ids = Vec::new();
        let mut payload = Vec::new();
        for issue in issues {
            let issue_id = issue
                .get("issue_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let detail = issue
                .get("detail")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let suggestion = issue
                .get("suggestion")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if issue_id.trim().is_empty()
                || detail.trim().is_empty()
                || suggestion.trim().is_empty()
            {
                return Err(fail("invalid_issue"));
            }
            let issue_id = issue_id.trim().to_string();
            if issue_ids.contains(&issue_id) {
                return Err(fail("duplicate_issue_id"));
            }
            if let Some(value) = issue.get("chapter").and_then(|v| v.as_i64()) {
                if value != chapter {
                    return Err(fail("issue_location_mismatch"));
                }
            }
            if let Some(value) = issue.get("index").and_then(|v| v.as_i64()) {
                if value != index {
                    return Err(fail("issue_location_mismatch"));
                }
            }
            let mut item = serde_json::Map::new();
            item.insert(
                "issue_id".to_string(),
                serde_json::Value::String(issue_id.clone()),
            );
            item.insert(
                "type".to_string(),
                serde_json::Value::String(
                    issue
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                ),
            );
            item.insert(
                "detail".to_string(),
                serde_json::Value::String(detail.trim().to_string()),
            );
            item.insert(
                "suggestion".to_string(),
                serde_json::Value::String(suggestion.trim().to_string()),
            );
            for field in ["consistency", "arbitration", "evidence_refs"] {
                if let Some(value) = issue.get(field) {
                    item.insert(field.to_string(), value.clone());
                }
            }
            issue_ids.push(issue_id);
            payload.push(serde_json::Value::Object(item));
        }
        let before_hash = Self::target_hash(current_target);
        let or_none = |value: &str| {
            if value.trim().is_empty() {
                "(none)".to_string()
            } else {
                value.trim().to_string()
            }
        };
        let mut extra = BTreeMap::new();
        let system = self
            .agent
            .render("review_fixer_system", &extra)
            .map_err(|_| fail("render_failed"))?;
        extra.insert("style".to_string(), or_none(style));
        extra.insert("book_synopsis".to_string(), or_none(book_synopsis));
        extra.insert("chapter_digest".to_string(), or_none(chapter_digest));
        extra.insert(
            "glossary".to_string(),
            render_glossary(relevant_glossary, &self.agent.src),
        );
        extra.insert("nearby_pairs".to_string(), fixer_nearby_text(nearby_pairs));
        extra.insert(
            "issues_json".to_string(),
            serde_json::Value::Array(payload).to_string(),
        );
        extra.insert("segment_ref".to_string(), segment_ref.to_string());
        extra.insert("before_hash".to_string(), before_hash.clone());
        extra.insert(
            "issue_ids_json".to_string(),
            serde_json::Value::Array(
                issue_ids
                    .iter()
                    .map(|id| serde_json::Value::String(id.clone()))
                    .collect(),
            )
            .to_string(),
        );
        extra.insert("source".to_string(), source.to_string());
        extra.insert("current_target".to_string(), current_target.to_string());
        let user = self
            .agent
            .render("review_fixer_user", &extra)
            .map_err(|_| fail("render_failed"))?;
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
        let raw = client
            .complete(&messages, &self.operation, true, None)
            .map_err(|error| fail(&format!("model_error:{error}")))?;
        let parsed = parse_json_result(&raw).map_err(|_| fail("malformed_json"))?;
        if !parsed.safe_for_complete_payload() {
            return Err(fail("unsafe_json_repair"));
        }
        let data = parsed
            .value
            .as_object()
            .ok_or_else(|| fail("response_not_object"))?;
        let expected: std::collections::BTreeSet<&str> = [
            "segment_ref",
            "before_hash",
            "issue_ids",
            "replacement",
            "complete",
        ]
        .into_iter()
        .collect();
        let got: std::collections::BTreeSet<&str> = data.keys().map(String::as_str).collect();
        if got != expected {
            return Err(fail("unexpected_fields"));
        }
        if data.get("complete") != Some(&serde_json::Value::Bool(true)) {
            return Err(fail("completion_marker_missing"));
        }
        if data.get("segment_ref").and_then(|v| v.as_str()) != Some(segment_ref) {
            return Err(fail("segment_ref_mismatch"));
        }
        if data.get("before_hash").and_then(|v| v.as_str()) != Some(before_hash.as_str()) {
            return Err(fail("before_hash_mismatch"));
        }
        let returned: Vec<String> = data
            .get("issue_ids")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let unique: std::collections::BTreeSet<&String> = returned.iter().collect();
        if unique.len() != returned.len() || returned.is_empty() && !issue_ids.is_empty() {
            // Empty model list with required ids falls through to mismatch
            // below; duplicates fail here.
            if unique.len() != returned.len() {
                return Err(fail("invalid_issue_ids"));
            }
        }
        let want: std::collections::BTreeSet<&String> = issue_ids.iter().collect();
        if unique != want {
            return Err(fail("issue_ids_mismatch"));
        }
        let replacement = data
            .get("replacement")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        if replacement.is_empty() {
            return Err(fail("empty_replacement"));
        }
        if replacement == current_target.trim() {
            return Err(fail("unchanged_replacement"));
        }
        let required = dialogue_quote_pairs(source).min(dialogue_quote_pairs(current_target));
        if dialogue_quote_pairs(&replacement) < required {
            return Err(fail("dropped_dialogue_quotes"));
        }
        Ok(ProvisionalPatch {
            patch_id: fixer_patch_id(
                round_number,
                segment_ref,
                &before_hash,
                &replacement,
                &issue_ids,
            ),
            round: round_number,
            segment_ref: segment_ref.to_string(),
            chapter,
            index,
            before_hash,
            before: current_target.to_string(),
            after: replacement,
            issue_ids,
        })
    }
}
