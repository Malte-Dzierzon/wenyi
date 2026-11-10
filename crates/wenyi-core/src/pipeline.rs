//! One-book translation pipeline. Oracle: `pipeline/preparation.py`,
//! `pipeline/translation.py`, `pipeline/translation_batch.py`,
//! `pipeline/context.py`, `pipeline/title_translation.py`.
//!
//! `Pipeline` owns config, resolved languages, frozen prompt plans, and
//! agents for one run. All persistence goes through `store` + `glossary`;
//! all model calls go through the injected [`ModelClient`](crate::pipeline_agents::ModelClient).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rusqlite::Connection;

use crate::config::Config;
use crate::document::models::Document;
use crate::document::{hash, segment};
use crate::glossary::store::GlossaryTerm;
use crate::glossary::{self, terms_in};
use crate::i18n::policy::{self, PolicyPlan};
use crate::jobs::ProgressEvent;
use crate::model::{BookId, ChapterId, ProjectId, SegmentId};
use crate::pipeline_agents::{
    numbered, Analyzer, AnnotationItem, GlossaryExtractor, ModelClient, Polisher, Synopsizer,
    TitleTranslator, TranslatedEvidence, Translator,
};
use crate::{store, Error, Result};

/// Progress callback `(done, total, message)`.
pub type ProgressFn<'a> = &'a mut dyn FnMut(i64, i64, &str);

/// Reborrowable progress sink (avoids `Option<&mut>` borrow fights).
pub struct Progress<'a> {
    /// Inner callback, if any.
    pub inner: Option<ProgressFn<'a>>,
}

impl<'a> Progress<'a> {
    /// Empty sink.
    pub fn none() -> Self {
        Self { inner: None }
    }

    /// Emit one update when a callback is present.
    pub fn emit(&mut self, done: i64, total: i64, message: &str) {
        if let Some(callback) = self.inner.as_deref_mut() {
            callback(done, total, message);
        }
    }
}

/// Rolling recent-translation context (oracle `RollingContext`).
#[derive(Debug, Clone)]
pub struct RollingContext {
    /// Recent nonempty targets, oldest first.
    pub recent_targets: Vec<String>,
    /// Maximum retained paragraphs (floor 40 in the oracle).
    pub max_recent_keep: usize,
}

impl RollingContext {
    /// Empty context with the configured retention.
    pub fn new(max_recent_keep: usize) -> Self {
        Self {
            recent_targets: Vec::new(),
            max_recent_keep,
        }
    }

    /// Restore persisted context with at least the required capacity.
    pub fn from_json(text: Option<&str>, min_recent_keep: usize) -> Self {
        let mut context = Self::new(min_recent_keep.max(40));
        let Some(text) = text else {
            return context;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            return context;
        };
        let persisted = value
            .get("max_recent_keep")
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(40);
        context.max_recent_keep = persisted.max(min_recent_keep);
        let targets: Vec<String> = value
            .get("recent_targets")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let keep = context.max_recent_keep;
        context.recent_targets = targets
            .into_iter()
            .rev()
            .take(keep)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        context
    }

    /// Serialize for persistence.
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "recent_targets": self.recent_targets,
            "max_recent_keep": self.max_recent_keep,
        })
        .to_string()
    }

    /// Render the last `n` translations (oracle `render`).
    pub fn render(&self, n_recent: usize) -> String {
        if n_recent == 0 {
            return String::new();
        }
        let start = self.recent_targets.len().saturating_sub(n_recent);
        self.recent_targets[start..].join("\n")
    }

    /// Append nonempty translations, retaining the configured tail.
    pub fn add_targets(&mut self, targets: &[String]) {
        for target in targets {
            if !target.trim().is_empty() {
                self.recent_targets.push(target.clone());
            }
        }
        let len = self.recent_targets.len();
        if len > self.max_recent_keep {
            self.recent_targets.drain(..len - self.max_recent_keep);
        }
    }
}

/// Sample style text from beginning/middle/end (oracle `sample_text`).
pub fn sample_text(document: &Document, labeled: bool) -> String {
    let texts: Vec<String> = document
        .chapters
        .iter()
        .map(|chapter| {
            chapter
                .segments
                .iter()
                .filter(|segment| !segment.source.trim().is_empty())
                .map(|segment| segment.source.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|text| text.chars().count() > 200)
        .collect();
    if texts.is_empty() {
        let joined: String = document
            .chapters
            .iter()
            .take(2)
            .flat_map(|chapter| {
                chapter
                    .segments
                    .iter()
                    .filter(|segment| !segment.source.trim().is_empty())
                    .map(|segment| segment.source.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n");
        return joined.chars().take(6000).collect();
    }
    if !labeled {
        return texts[0].chars().take(6000).collect();
    }
    let picks = [
        (0, "Opening sample"),
        (texts.len() / 2, "Middle sample"),
        (texts.len() - 1, "Ending sample"),
    ];
    let mut parts = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, tag) in picks {
        if !seen.insert(index) {
            continue;
        }
        let text = &texts[index];
        let chunk: String = if tag == "Ending sample" {
            let chars: Vec<char> = text.chars().collect();
            let start = chars.len().saturating_sub(2800);
            chars[start..].iter().collect()
        } else {
            text.chars().take(2800).collect()
        };
        parts.push(format!("【{tag}】\n{chunk}"));
    }
    parts.join("\n\n")
}

/// Whether a digest counts as complete (oracle `_digest_complete`).
pub fn digest_complete(digest: &str, complete: bool) -> bool {
    if digest.trim().is_empty() {
        return false;
    }
    if complete {
        return true;
    }
    let cleaned = digest
        .trim()
        .trim_end_matches(['”', '"', '\'', '」', '』', '）', ')', ']', '}']);
    cleaned
        .chars()
        .last()
        .is_some_and(|c| matches!(c, '。' | '．' | '.' | '！' | '？' | '!' | '?' | '…'))
}

/// Flatten title whitespace (oracle `title_translation._flat`).
pub fn flat_title(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Detect the source language via the model (oracle `detect_language_ai`).
pub fn detect_language(
    client: &mut dyn ModelClient,
    plan: &PolicyPlan,
    document: &Document,
) -> Result<String> {
    let sample: String = sample_text(document, false).chars().take(1500).collect();
    if sample.trim().is_empty() {
        return Ok(String::new());
    }
    let system = plan.render("language_detector_system", &BTreeMap::new())?;
    let messages = vec![
        crate::llm::compat::Message {
            role: "system".to_string(),
            content: system,
        },
        crate::llm::compat::Message {
            role: "user".to_string(),
            content: sample,
        },
    ];
    let text = client.complete(&messages, "language.detect", true, None)?;
    let value = crate::llm::json::parse_json_loose(&text).map_err(|e| {
        Error::Config(format!(
            "Cannot parse language detection JSON: {}",
            e.preview
        ))
    })?;
    let code = value.get("language").and_then(|v| v.as_str()).unwrap_or("");
    Ok(crate::i18n::normalize_language(code))
}

/// One-book pipeline: preparation + translation + titles.
pub struct Pipeline<'a> {
    /// Effective configuration.
    pub config: Config,
    /// Resolved source language.
    pub src: String,
    /// Resolved target language.
    pub tgt: String,
    /// Model client (fake in goldens, routed in production).
    pub client: &'a mut dyn ModelClient,
    /// Usage ledger (oracle `client.usage`).
    pub usage: crate::llm::usage::UsageTracker,
}

impl<'a> Pipeline<'a> {
    /// Bind config + client; languages resolve during preparation.
    pub fn new(config: Config, client: &'a mut dyn ModelClient) -> Self {
        let src = config.source_lang.clone();
        let tgt = config.target_lang.clone();
        Self {
            config,
            src,
            tgt,
            client,
            usage: crate::llm::usage::UsageTracker::new(),
        }
    }

    /// Frozen plan for one phase (oracle `config.language_policy`).
    pub fn plan(&self, phase: &str) -> Result<PolicyPlan> {
        let best_of_three =
            self.config.pipeline.translation_mode == crate::config::TranslationMode::BestOfThree;
        let groups = policy::task_groups(
            phase,
            self.config.pipeline.book_understanding,
            self.config.pipeline.polish,
            best_of_three,
        )?;
        let refs: Vec<&str> = groups.iter().map(|s| s as &str).collect();
        policy::resolve_policy(
            &self.src,
            &self.tgt,
            phase,
            &refs,
            &self.config.honorific_strategy,
        )
    }

    /// Record fixed metered usage per fake call (test helper mirroring
    /// `MeteredFakeClient`: 5 prompt + 3 completion tokens).
    pub fn record_metered(&self, operation: &str, tier: &str) {
        self.usage.record(
            Some(crate::llm::usage::UsageSample {
                prompt_tokens: 5,
                completion_tokens: 3,
                total_tokens: 8,
                cache_hit_tokens: 0,
                cache_miss_tokens: 5,
                completion_ms: 0,
            }),
            &crate::llm::usage::UsageAttribution {
                tier: tier.to_string(),
                stage: operation.to_string(),
                provider: "unknown".to_string(),
                model: "unknown".to_string(),
            },
            &BTreeMap::new(),
        );
    }

    /// Prepare one input file into a project/book (oracle
    /// `prepare_for_translation`).
    pub fn prepare(
        &mut self,
        conn: &Connection,
        project_id: &ProjectId,
        input_path: &Path,
        progress: &mut Progress<'_>,
    ) -> Result<(BookId, String)> {
        let ext = input_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase();
        let source_hash = hash::source_sha256(input_path)?;
        let existing = store::list_books(conn, project_id)?;
        if let Some(book) = existing.first() {
            self.src = self.config.source_lang.clone();
            self.tgt = self.config.target_lang.clone();
            let synopsis = self.ensure_understanding(conn, &book.id, &mut *progress)?;
            return Ok((book.id.clone(), synopsis));
        }
        let mut document = match ext.as_str() {
            "epub" => crate::document::reader::read_epub(input_path, &self.src, &self.tgt)?,
            "html" | "htm" | "xhtml" => {
                crate::document::html::read_html(input_path, &self.src, &self.tgt, None)?
            }
            _ => crate::document::text::read_text(input_path, &self.src, &self.tgt)?,
        };
        if self.config.segment.max_tokens_per_segment > 0 {
            segment::split_long_segments(
                &mut document.chapters,
                self.config.segment.max_tokens_per_segment.max(0) as usize,
            );
        }
        if self.src == "auto" || self.src.is_empty() {
            progress.emit(0, 0, "Detecting language…");
            let plan = self.plan("analysis")?;
            let detected = detect_language(self.client, &plan, &document)?;
            self.record_metered("language.detect", "fast");
            if detected.is_empty() {
                return Err(Error::Config(
                    "Source language detection returned no supported language. Set \
                     language.source in config.yaml to a supported code, such as \
                     ja/en/zh-Hant/ko/fr/de/es."
                        .to_string(),
                ));
            }
            self.src = detected;
            self.config.source_lang = self.src.clone();
        }
        if self.src == self.tgt {
            return Err(Error::Config(format!(
                "Source and target languages are identical ({src}); no translation is needed. \
                 Change language.source or language.target in config.yaml.",
                src = self.src
            )));
        }
        document.source_lang = self.src.clone();
        document.target_lang = self.tgt.clone();
        let book_id = BookId::new(format!("book-{}", &source_hash[..16]))?;
        store::create_book(conn, &book_id, project_id, 0, &document.title)?;
        for (ordinal, chapter) in document.chapters.iter().enumerate() {
            let chapter_id = ChapterId::new(format!("{}-ch{ordinal:04}", book_id.as_str()))?;
            store::create_chapter(conn, &chapter_id, &book_id, ordinal as i64, &chapter.title)?;
            let segments: Vec<(SegmentId, String)> = chapter
                .segments
                .iter()
                .enumerate()
                .filter(|(_, segment)| !segment.source.trim().is_empty())
                .map(|(index, segment)| {
                    (
                        SegmentId::new(format!("{}-s{index:05}", chapter_id.as_str()))
                            .expect("segment id"),
                        segment.source.clone(),
                    )
                })
                .collect();
            // Persist kind/anchor/cont/meta alongside sources: insert the
            // rows first, then patch format columns.
            let pairs: Vec<(SegmentId, String)> = segments
                .iter()
                .map(|(id, source)| (id.clone(), source.clone()))
                .collect();
            store::insert_segments(conn, &chapter_id, &pairs)?;
            for (ordinal, doc_segment) in chapter
                .segments
                .iter()
                .filter(|s| !s.source.trim().is_empty())
                .enumerate()
            {
                if doc_segment.kind != "text"
                    || doc_segment.anchor.is_some()
                    || doc_segment.cont
                    || !doc_segment.meta.is_empty()
                {
                    store::update_segment_format(
                        conn,
                        &chapter_id,
                        ordinal as i64,
                        &doc_segment.kind,
                        doc_segment.anchor.as_deref().unwrap_or(""),
                        doc_segment.resource_href.as_deref().unwrap_or(""),
                        doc_segment.cont,
                        &serde_json::Value::Object(doc_segment.meta.clone()).to_string(),
                    )?;
                }
            }
        }
        if self.config.pipeline.book_understanding {
            progress.emit(0, 0, "Prescanning chapter digests");
            self.ensure_digests(conn, &book_id, &mut *progress)?;
        }
        progress.emit(0, 0, "Analyzing book style…");
        let sample = sample_text(&document, true);
        let analysis_plan = self.plan("analysis")?;
        let analyzer = Analyzer::new(&self.src, &self.tgt, analysis_plan.clone(), &self.config);
        let mut analysis = if sample.is_empty() {
            serde_json::Map::new()
        } else {
            let data = analyzer.analyze(self.client, &sample)?;
            self.record_metered("analysis.style", "strong");
            data
        };
        analysis.insert(
            "language_policy".to_string(),
            serde_json::Value::String(format!(
                "language-policies/{}.json",
                analysis_plan.fingerprint
            )),
        );
        analysis.insert(
            "style_policy".to_string(),
            serde_json::Value::String(analysis_plan.task_fingerprint("analyzer")),
        );
        let seed_terms = analyzer.seed_terms(&analysis);
        for term in &seed_terms {
            glossary::upsert_term(conn, project_id, &self.src, &self.tgt, term, Some(0))?;
        }
        store::save_analysis(
            conn,
            &book_id,
            &serde_json::Value::Object(analysis).to_string(),
        )?;
        let keep = self.config.pipeline.rolling_context_segments.max(0) as usize;
        let context = RollingContext::new(keep.max(40));
        store::save_context(conn, &book_id, &context.to_json())?;
        let synopsis = self.ensure_understanding(conn, &book_id, progress)?;
        Ok((book_id, synopsis))
    }

    /// Prescan chapter digests, skipping complete ones (oracle
    /// `_ensure_chapter_digests`).
    pub fn ensure_digests(
        &mut self,
        conn: &Connection,
        book_id: &BookId,
        progress: &mut Progress<'_>,
    ) -> Result<Vec<String>> {
        let chapters = store::list_chapters(conn, book_id, 0, i64::MAX)?;
        let plan = self.plan("analysis")?;
        let fingerprint = plan.task_fingerprint("chapter_digest");
        let synopsizer = Synopsizer::new(&self.src, &self.tgt, plan, &self.config);
        // Collect todo first (immutable pass), then digest serially. The
        // oracle uses a thread pool but persists per chapter; serial keeps
        // lock scopes short and output order stable.
        let mut todo: Vec<(ChapterId, String)> = Vec::new();
        let mut sources: Vec<(ChapterId, String)> = Vec::new();
        for chapter in &chapters {
            let segments = store::list_segments(conn, &chapter.id, 0, i64::MAX)?;
            let non_empty: Vec<&store::Segment> = segments
                .iter()
                .filter(|segment| !segment.source.trim().is_empty())
                .collect();
            if non_empty.is_empty() {
                continue;
            }
            let source = non_empty
                .iter()
                .map(|segment| segment.source.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            sources.push((chapter.id.clone(), source.clone()));
            let saved = store::load_digest(conn, &chapter.id)?;
            let complete = saved.as_ref().is_some_and(|(digest, done, policy)| {
                digest_complete(digest, *done) && policy == &fingerprint
            });
            if !complete {
                todo.push((chapter.id.clone(), source));
            }
        }
        let total = todo.len();
        for (index, (chapter_id, source)) in todo.into_iter().enumerate() {
            progress.emit(index as i64, total as i64, "Prescanning chapter digests");
            let digest = synopsizer.digest_chapter(self.client, &source);
            self.record_metered("synopsis.chapter", "fast");
            let digest = digest.trim().to_string();
            if digest.is_empty() {
                let ordinal = chapters
                    .iter()
                    .find(|chapter| chapter.id == chapter_id)
                    .map(|chapter| chapter.ordinal)
                    .unwrap_or(0);
                return Err(Error::Config(format!(
                    "Chapter digests could not be generated for chapters: {ordinal}. \
                     Retry prepare/translate; completed digests have been saved."
                )));
            }
            store::save_digest(conn, &chapter_id, &digest, true, &fingerprint)?;
            progress.emit(
                index as i64 + 1,
                total as i64,
                "Prescanning chapter digests",
            );
        }
        let mut out = Vec::new();
        for (chapter_id, _) in &sources {
            let saved = store::load_digest(conn, chapter_id)?.unwrap_or_default();
            out.push(saved.0);
        }
        Ok(out)
    }

    /// Build or reuse the whole-book synopsis (oracle
    /// `ensure_understanding`).
    pub fn ensure_understanding(
        &mut self,
        conn: &Connection,
        book_id: &BookId,
        progress: &mut Progress<'_>,
    ) -> Result<String> {
        if !self.config.pipeline.book_understanding {
            return Ok(String::new());
        }
        let chapters = store::list_chapters(conn, book_id, 0, i64::MAX)?;
        let digests = self.ensure_digests(conn, book_id, &mut *progress)?;
        if digests.is_empty() {
            return Ok(String::new());
        }
        let analysis_text =
            store::load_analysis(conn, book_id)?.unwrap_or_else(|| "{}".to_string());
        let analysis: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&analysis_text).unwrap_or_default();
        let style = Analyzer::style_brief(&analysis);
        let plan = self.plan("analysis")?;
        let mut pairs: Vec<(i64, String)> = Vec::new();
        for chapter in &chapters {
            let segments = store::list_segments(conn, &chapter.id, 0, i64::MAX)?;
            if segments.iter().any(|s| !s.source.trim().is_empty()) {
                let digest = store::load_digest(conn, &chapter.id)?.unwrap_or_default().0;
                pairs.push((chapter.ordinal, digest));
            }
        }
        let inputs = serde_json::json!({
            "language_policy": plan.task_fingerprint("book_synopsis"),
            "chapters": pairs.iter().map(|(index, digest)| serde_json::json!([index, digest])).collect::<Vec<_>>(),
            "style": style,
            "source_lang": self.src,
            "target_lang": self.tgt,
        });
        let fingerprint = policy::content_hash(&inputs);
        let metadata = serde_json::json!({"version": 1, "inputs_sha256": fingerprint});
        let meta_text = serde_json::to_string(&metadata).unwrap_or_default();
        if let Some((synopsis, saved_meta)) = store::load_synopsis(conn, book_id)? {
            if !synopsis.trim().is_empty() && saved_meta == meta_text {
                return Ok(synopsis);
            }
        }
        progress.emit(0, 0, "Generating whole-book synopsis…");
        let synopsizer = Synopsizer::new(&self.src, &self.tgt, plan, &self.config);
        let synopsis = synopsizer.book_synopsis(self.client, &digests, &style);
        self.record_metered("synopsis.book", "fast");
        if synopsis.is_empty() {
            return Ok(String::new());
        }
        store::save_synopsis(conn, book_id, &synopsis, &meta_text)?;
        Ok(synopsis)
    }
}

/// Translation progress counts + history for one run.
pub struct RunInputs {
    /// Translated evidence by (chapter, segment).
    pub history: BTreeMap<(i64, i64), TranslatedEvidence>,
    /// Whole-book source corpus for recurrence checks.
    pub source_corpus: String,
    /// Total translatable segments.
    pub total: i64,
    /// Completed segments at run start.
    pub done: i64,
}

impl<'a> Pipeline<'a> {
    /// Translate pending chapters serially (oracle `TranslationService::run`).
    #[allow(clippy::too_many_arguments)] // Mirrors the oracle run signature.
    pub fn translate(
        &mut self,
        conn: &Connection,
        project_id: &ProjectId,
        book_id: &BookId,
        book_synopsis: &str,
        only_chapter: Option<i64>,
        progress: &mut Progress<'_>,
        events: &mut Vec<serde_json::Value>,
    ) -> Result<()> {
        let chapters = store::list_chapters(conn, book_id, 0, i64::MAX)?;
        if let Some(only) = only_chapter {
            if !chapters.iter().any(|chapter| chapter.ordinal == only) {
                let max = chapters.iter().map(|chapter| chapter.ordinal).max();
                let valid = max
                    .map(|m| format!("0–{m}"))
                    .unwrap_or_else(|| "no translatable chapters".to_string());
                return Err(Error::Config(format!(
                    "Chapter index {only} does not exist; available range: {valid}"
                )));
            }
        }
        let targets: Vec<i64> = match only_chapter {
            Some(only) => vec![only],
            None => store::pending_chapters(conn, book_id)?,
        };
        let context_json = store::load_context(conn, book_id)?;
        let mut context = RollingContext::from_json(
            context_json.as_deref(),
            self.config.pipeline.rolling_context_segments.max(0) as usize,
        );
        let analysis_text =
            store::load_analysis(conn, book_id)?.unwrap_or_else(|| "{}".to_string());
        let analysis: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&analysis_text).unwrap_or_default();
        let style = Analyzer::style_brief(&analysis);
        let inputs = self.prepare_run_inputs(conn, book_id, only_chapter)?;
        let mut done = inputs.done;
        events.push(serde_json::json!({
            "event": "translate_run_started",
            "only_chapter": only_chapter,
            "chapters": targets,
            "total_segments": inputs.total,
            "allow_empty_translations": false,
        }));
        let mut history = inputs.history;
        for ordinal in &targets {
            done = self.translate_chapter(
                conn,
                project_id,
                book_id,
                *ordinal,
                &mut context,
                &style,
                book_synopsis,
                &mut history,
                &inputs.source_corpus,
                &mut *progress,
                done,
                inputs.total,
                events,
            )?;
            store::save_context(conn, book_id, &context.to_json())?;
        }
        if store::pending_chapters(conn, book_id)?.is_empty() {
            self.translate_titles(conn, project_id, book_id, &mut *progress, events)?;
        }
        events.push(serde_json::json!({
            "event": "translate_run_finished",
            "total_segments": inputs.total,
        }));
        if inputs.total > 0 {
            progress.emit(inputs.total, inputs.total, "Translation complete");
        }
        Ok(())
    }

    /// Load history + corpus + progress counts in one pass (oracle
    /// `prepare_run_inputs`).
    pub fn prepare_run_inputs(
        &mut self,
        conn: &Connection,
        book_id: &BookId,
        only_chapter: Option<i64>,
    ) -> Result<RunInputs> {
        let chapters = store::list_chapters(conn, book_id, 0, i64::MAX)?;
        let mut history: BTreeMap<(i64, i64), TranslatedEvidence> = BTreeMap::new();
        let mut source_parts: Vec<String> = Vec::new();
        let mut total = 0i64;
        let mut done = 0i64;
        let max_tokens = self.config.segment.max_tokens_per_batch.max(0) as usize;
        for chapter in &chapters {
            let segments = store::list_segments(conn, &chapter.id, 0, i64::MAX)?;
            let non_empty: Vec<&store::Segment> = segments
                .iter()
                .filter(|segment| !segment.source.trim().is_empty())
                .collect();
            for segment in &non_empty {
                source_parts.push(segment.source.clone());
                if let Some(target) = &segment.target {
                    if !target.trim().is_empty() {
                        history.insert(
                            (chapter.ordinal, segment.ordinal),
                            TranslatedEvidence {
                                chapter: chapter.ordinal,
                                segment: segment.ordinal,
                                source: segment.source.clone(),
                                target: target.trim().to_string(),
                            },
                        );
                    }
                }
            }
            if only_chapter.is_none_or(|only| only == chapter.ordinal) {
                total += non_empty.len() as i64;
                for batch in resume_batches(&non_empty, max_tokens) {
                    if batch.iter().all(|segment| segment.target.is_some()) {
                        done += batch.len() as i64;
                    }
                }
            }
        }
        Ok(RunInputs {
            history,
            source_corpus: source_parts.join("\n"),
            total,
            done,
        })
    }

    /// Translate one chapter batch by batch (oracle `translate_chapter`).
    #[allow(clippy::too_many_arguments)]
    pub fn translate_chapter(
        &mut self,
        conn: &Connection,
        project_id: &ProjectId,
        book_id: &BookId,
        ordinal: i64,
        context: &mut RollingContext,
        style: &str,
        book_synopsis: &str,
        history: &mut BTreeMap<(i64, i64), TranslatedEvidence>,
        source_corpus: &str,
        progress: &mut Progress<'_>,
        mut done: i64,
        total: i64,
        events: &mut Vec<serde_json::Value>,
    ) -> Result<i64> {
        let _ = book_id;
        let chapters = store::list_chapters(conn, book_id, 0, i64::MAX)?;
        let chapter = chapters
            .iter()
            .find(|chapter| chapter.ordinal == ordinal)
            .ok_or_else(|| Error::NotFound {
                kind: "chapter",
                id: ordinal.to_string(),
            })?
            .clone();
        let segments = store::list_segments(conn, &chapter.id, 0, i64::MAX)?;
        let text_segs: Vec<store::Segment> = segments
            .into_iter()
            .filter(|segment| !segment.source.trim().is_empty())
            .collect();
        if text_segs.is_empty() {
            store::set_chapter_status(conn, &chapter.id, "done")?;
            return Ok(done);
        }
        let chapter_digest = store::load_digest(conn, &chapter.id)?
            .map(|(digest, _, _)| digest)
            .unwrap_or_default();
        let max_tokens = self.config.segment.max_tokens_per_batch.max(0) as usize;
        let refs: Vec<&store::Segment> = text_segs.iter().collect();
        let batches = resume_batches(&refs, max_tokens);
        let label = if chapter.title.trim().is_empty() {
            format!("Chapter {}", ordinal + 1)
        } else {
            chapter.title.clone()
        };
        progress.emit(done, total, &label);
        let mut checkpoints = store::completed_batch_glossary_keys(conn, &chapter.id)?;
        let mut term_snapshot = glossary::all_terms(conn, project_id, &self.src, &self.tgt)?;
        let mut term_snapshot_stale = false;
        let plan = self.plan("translation")?;
        let mut seg_base = 0usize;
        for (batch_position, batch) in batches.iter().enumerate() {
            let batch_start = seg_base;
            let key = store::batch_glossary_key(batch_start, batch.len());
            if batch.iter().all(|segment| segment.target.is_some()) {
                let targets: Vec<String> = batch
                    .iter()
                    .map(|segment| segment.target.clone().unwrap_or_default())
                    .collect();
                context.add_targets(&targets);
                if !checkpoints.contains(&key) {
                    self.extract_batch_glossary(
                        conn,
                        project_id,
                        ordinal,
                        batch_start,
                        batch,
                        history,
                        source_corpus,
                        &term_snapshot,
                    )?;
                    store::complete_batch_glossary_key(conn, &chapter.id, &key)?;
                    checkpoints.insert(key);
                    term_snapshot_stale = true;
                }
                events.push(serde_json::json!({
                    "event": "batch_skipped",
                    "chapter": ordinal,
                    "start_index": batch_start,
                    "count": batch.len(),
                    "reason": "already_translated",
                }));
                seg_base += batch.len();
                progress.emit(done, total, &label);
                continue;
            }
            if term_snapshot_stale {
                term_snapshot = glossary::all_terms(conn, project_id, &self.src, &self.tgt)?;
            }
            let ctx_text =
                context.render(self.config.pipeline.rolling_context_segments.max(0) as usize);
            let next_index = batch_start + batch.len();
            let next_source = text_segs
                .get(next_index)
                .map(|segment| segment.source.as_str())
                .unwrap_or("");
            let joined = batch
                .iter()
                .map(|segment| segment.source.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let relevant = terms_in(&term_snapshot, &joined);
            let sources: Vec<String> = batch.iter().map(|segment| segment.source.clone()).collect();
            let empty_annotations: Vec<Vec<AnnotationItem>> =
                batch.iter().map(|_| Vec::new()).collect();
            let mut translator = Translator::new(&self.src, &self.tgt, plan.clone(), &self.config);
            let translated = translator.translate_batch(
                self.client,
                &sources,
                &relevant,
                style,
                &ctx_text,
                book_synopsis,
                &chapter_digest,
                &empty_annotations,
                next_source,
                false,
            )?;
            self.record_metered("translation.body", "strong");
            let translated: Vec<String> = translated
                .iter()
                .map(|target| crate::document::ruby::strip_markers(target))
                .collect();
            let mut before_polish: Vec<Option<String>> = batch.iter().map(|_| None).collect();
            let mut polish_failed = false;
            let mut targets = translated.clone();
            if self.config.pipeline.polish {
                before_polish = translated.iter().map(|t| Some(t.clone())).collect();
                let turn = translator.last_batch_turn.clone();
                let indices = translator.last_batch_indices.clone();
                let mut polished: Option<Vec<String>> = None;
                if let (Some(turn), Some(indices)) = (turn, indices) {
                    let polisher = Polisher::new(&self.src, &self.tgt, plan.clone(), &self.config);
                    if let Some(continued) =
                        polisher.polish_continue(self.client, &turn, indices.len(), next_source)
                    {
                        self.record_metered("polish.body", "strong");
                        if continued.len() == indices.len() {
                            let mut restored = translated.clone();
                            for (position, text) in indices.iter().zip(continued) {
                                restored[*position] = crate::document::ruby::strip_markers(&text);
                            }
                            polished = Some(restored);
                        }
                    }
                }
                if polished.is_none() {
                    let mut polisher =
                        Polisher::new(&self.src, &self.tgt, plan.clone(), &self.config);
                    let fresh =
                        polisher.polish(self.client, &translated, &relevant, style, next_source);
                    self.record_metered("polish.body", "strong");
                    polish_failed = polisher.last_polish_failed;
                    polished = Some(fresh);
                }
                if let Some(polished) = polished {
                    if polished.len() == targets.len() {
                        targets = polished;
                    }
                }
            }
            for (position, segment) in batch.iter().enumerate() {
                store::set_target_with_baseline(
                    conn,
                    &segment.id,
                    Some(&targets[position]),
                    before_polish[position].as_deref(),
                    "translate",
                )?;
            }
            context.add_targets(&targets);
            events.push(serde_json::json!({
                "event": "batch_translated",
                "chapter": ordinal,
                "start_index": batch_start,
                "count": batch.len(),
                "polished": self.config.pipeline.polish,
                "polish_failed": polish_failed,
            }));
            done += batch.len() as i64;
            seg_base += batch.len();
            progress.emit(done, total, &label);
            self.extract_batch_glossary(
                conn,
                project_id,
                ordinal,
                batch_start,
                batch,
                history,
                source_corpus,
                &term_snapshot,
            )?;
            for (offset, segment) in batch.iter().enumerate() {
                let target = targets[offset].trim();
                if target.is_empty() {
                    continue;
                }
                history.insert(
                    (ordinal, segment.ordinal),
                    TranslatedEvidence {
                        chapter: ordinal,
                        segment: segment.ordinal,
                        source: segment.source.clone(),
                        target: target.to_string(),
                    },
                );
            }
            store::complete_batch_glossary_key(conn, &chapter.id, &key)?;
            checkpoints.insert(key);
            // The glossary may have changed; refresh before the next real
            // translation. No later batch needs it: skip the flag.
            term_snapshot_stale = batches[batch_position + 1..]
                .iter()
                .any(|later| later.iter().any(|segment| segment.target.is_none()));
        }
        // Chapter-wide extraction fallback for address/speech/fixed
        // expressions needing full-chapter context.
        let src_text = text_segs
            .iter()
            .map(|segment| segment.source.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let chapter_targets = store::list_segments(conn, &chapter.id, 0, i64::MAX)?
            .into_iter()
            .filter(|segment| !segment.source.trim().is_empty())
            .map(|segment| segment.target.unwrap_or_default())
            .collect::<Vec<_>>();
        let tgt_text = chapter_targets.join("\n");
        let history_vec: Vec<TranslatedEvidence> = history.values().cloned().collect();
        self.extract_and_store(
            conn,
            project_id,
            &src_text,
            &tgt_text,
            ordinal,
            &history_vec,
            Some((ordinal, text_segs.len() as i64)),
            Some(source_corpus),
            None,
        )?;
        store::set_chapter_status(conn, &chapter.id, "done")?;
        Ok(done)
    }

    /// Extract + align + store terms for one batch (oracle
    /// `extract_batch_glossary` + `extract_and_store`).
    #[allow(clippy::too_many_arguments)]
    pub fn extract_batch_glossary(
        &mut self,
        conn: &Connection,
        project_id: &ProjectId,
        ordinal: i64,
        batch_start: usize,
        batch: &[&store::Segment],
        history: &BTreeMap<(i64, i64), TranslatedEvidence>,
        source_corpus: &str,
        existing_terms: &[GlossaryTerm],
    ) -> Result<()> {
        let src_text = batch
            .iter()
            .map(|segment| segment.source.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        // Read fresh targets (just persisted above).
        let chapter = store::list_chapters(conn, &self.book_of(conn, project_id)?, 0, i64::MAX)?
            .into_iter()
            .find(|chapter| chapter.ordinal == ordinal)
            .ok_or_else(|| Error::NotFound {
                kind: "chapter",
                id: ordinal.to_string(),
            })?;
        let fresh = store::list_segments(conn, &chapter.id, 0, i64::MAX)?
            .into_iter()
            .filter(|segment| !segment.source.trim().is_empty())
            .skip(batch_start)
            .take(batch.len())
            .map(|segment| segment.target.unwrap_or_default())
            .collect::<Vec<_>>();
        let tgt_text = fresh.join("\n");
        let history_vec: Vec<TranslatedEvidence> = history.values().cloned().collect();
        self.extract_and_store(
            conn,
            project_id,
            &src_text,
            &tgt_text,
            ordinal,
            &history_vec,
            Some((ordinal, batch_start as i64)),
            Some(source_corpus),
            Some(existing_terms),
        )?;
        Ok(())
    }

    /// Resolve the single book of a one-book project (Slice 4 scope).
    fn book_of(&self, conn: &Connection, project_id: &ProjectId) -> Result<BookId> {
        let books = store::list_books(conn, project_id)?;
        books
            .into_iter()
            .next()
            .map(|book| book.id)
            .ok_or_else(|| Error::NotFound {
                kind: "book",
                id: project_id.as_str().to_string(),
            })
    }

    /// Extract, align with first occurrences, and store (oracle
    /// `GlossaryExtractor::extract_and_store`).
    #[allow(clippy::too_many_arguments)]
    pub fn extract_and_store(
        &mut self,
        conn: &Connection,
        project_id: &ProjectId,
        source_text: &str,
        target_text: &str,
        ordinal: i64,
        history: &[TranslatedEvidence],
        before: Option<(i64, i64)>,
        source_corpus: Option<&str>,
        existing_terms: Option<&[GlossaryTerm]>,
    ) -> Result<()> {
        let plan = self.plan("translation")?;
        let mut extractor = GlossaryExtractor::new(&self.src, &self.tgt, plan, &self.config);
        let owned;
        let all_existing: &[GlossaryTerm] = match existing_terms {
            Some(terms) => terms,
            None => {
                owned = glossary::all_terms(conn, project_id, &self.src, &self.tgt)?;
                &owned
            }
        };
        let existing: Vec<GlossaryTerm> = match source_corpus {
            Some(corpus) => GlossaryExtractor::recurring_existing_terms(all_existing, corpus),
            None => all_existing.to_vec(),
        };
        let terms = extractor.extract(self.client, source_text, target_text, &existing);
        self.record_metered("glossary.extract", "cheap");
        let occurrences = match before {
            Some(position) => {
                let is_stored = |source: &str| {
                    glossary::get_term(conn, project_id, &self.src, &self.tgt, source)
                        .map(|term| term.is_some())
                        .unwrap_or(false)
                };
                extractor.first_occurrences(&terms, &is_stored, history, position)
            }
            None => BTreeMap::new(),
        };
        if !occurrences.is_empty() {
            self.record_metered("glossary.align_history", "cheap");
        }
        let (terms, _, _) =
            extractor.align_with_first_occurrences(self.client, terms, &occurrences);
        for term in &terms {
            let evidence = occurrences.get(&term.source);
            let mut term = term.clone();
            term.first_chapter = Some(match evidence {
                Some(found) => found.chapter,
                None => ordinal,
            });
            glossary::upsert_term(conn, project_id, &self.src, &self.tgt, &term, Some(ordinal))?;
        }
        Ok(())
    }

    /// Translate pending chapter titles (oracle `TitleTranslationService`).
    pub fn translate_titles(
        &mut self,
        conn: &Connection,
        project_id: &ProjectId,
        book_id: &BookId,
        progress: &mut Progress<'_>,
        events: &mut Vec<serde_json::Value>,
    ) -> Result<()> {
        let chapters = store::list_chapters(conn, book_id, 0, i64::MAX)?;
        // Heading reuse: translated heading whose source matches the chapter
        // title (Slice 4: no TOC entries yet; manifest chapters only).
        for chapter in &chapters {
            if chapter
                .title_translated
                .as_ref()
                .is_some_and(|t| !t.trim().is_empty())
            {
                continue;
            }
            let segments = store::list_segments(conn, &chapter.id, 0, i64::MAX)?;
            let heading = segments.iter().find(|segment| {
                segment.kind == "heading"
                    && !segment.anchor.is_empty()
                    && flat_title(&segment.source) == flat_title(&chapter.title)
            });
            if let Some(heading) = heading {
                if let Some(target) = &heading.target {
                    if !target.trim().is_empty() {
                        store::set_title_translated(conn, &chapter.id, Some(target.trim()))?;
                    }
                }
            }
        }
        let chapters = store::list_chapters(conn, book_id, 0, i64::MAX)?;
        let pending: Vec<(ChapterId, String)> = chapters
            .iter()
            .filter(|chapter| {
                !flat_title(&chapter.title).is_empty()
                    && !chapter
                        .title_translated
                        .as_ref()
                        .is_some_and(|t| !t.trim().is_empty())
            })
            .map(|chapter| (chapter.id.clone(), flat_title(&chapter.title)))
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        progress.emit(0, pending.len() as i64, "Translating chapter titles…");
        // 40-title / 4000-char request boundaries.
        let mut batches: Vec<Vec<(ChapterId, String)>> = Vec::new();
        let mut current: Vec<(ChapterId, String)> = Vec::new();
        let mut current_chars = 0usize;
        for item in &pending {
            if !current.is_empty()
                && (current.len() >= 40 || current_chars + item.1.chars().count() > 4000)
            {
                batches.push(std::mem::take(&mut current));
                current_chars = 0;
            }
            current_chars += item.1.chars().count();
            current.push(item.clone());
        }
        if !current.is_empty() {
            batches.push(current);
        }
        let plan = self.plan("translation")?;
        let translator = TitleTranslator::new(&self.src, &self.tgt, plan, &self.config);
        let term_snapshot = glossary::all_terms(conn, project_id, &self.src, &self.tgt)?;
        let mut completed = 0usize;
        for (batch_index, batch) in batches.iter().enumerate() {
            let titles: Vec<String> = batch.iter().map(|(_, title)| title.clone()).collect();
            let joined = titles.join(" ");
            let glossary_text = numbered(&[]);
            let _ = glossary_text;
            let relevant = terms_in(&term_snapshot, &joined);
            let glossary_rendered = crate::pipeline_agents::render_glossary(&relevant, &self.src);
            let translated = translator
                .translate(self.client, &titles, &glossary_rendered)
                .map_err(|error| {
                    events.push(serde_json::json!({
                        "event": "titles_translation_rejected",
                        "batch": batch_index,
                        "reason": "count_mismatch",
                        "expected": error.expected,
                        "actual": error.actual,
                    }));
                    Error::Config(error.to_string())
                })?;
            self.record_metered("translation.title", "cheap");
            for ((chapter_id, source), target) in batch.iter().zip(translated.iter()) {
                let final_title = if target.is_empty() { source } else { target };
                store::set_title_translated(conn, chapter_id, Some(final_title))?;
            }
            events.push(serde_json::json!({
                "event": "titles_translated",
                "batch": batch_index,
                "titles": batch.iter().zip(translated.iter()).map(|((_, source), target)| {
                    serde_json::json!({"source": source, "target": target})
                }).collect::<Vec<_>>(),
            }));
            completed += batch.len();
            progress.emit(
                completed as i64,
                pending.len() as i64,
                "Translating chapter titles",
            );
        }
        Ok(())
    }
}

/// Split token-budget batches again at completed/pending boundaries
/// (oracle `resume_batches`).
pub fn resume_batches<'b>(
    segments: &[&'b store::Segment],
    max_tokens: usize,
) -> Vec<Vec<&'b store::Segment>> {
    // Reuse document batching over borrowed sources.
    let owned: Vec<crate::document::models::Segment> = segments
        .iter()
        .map(|segment| {
            let mut doc = crate::document::models::Segment::text(0, segment.source.clone());
            doc.target = segment.target.clone();
            doc
        })
        .collect();
    let raw = segment::batch_segments(&owned, max_tokens);
    // Map back to store segments by position: batch_segments preserves
    // order, so regroup the original refs by lengths.
    let mut out: Vec<Vec<&'b store::Segment>> = Vec::new();
    let mut cursor = 0usize;
    for group in &raw {
        let group_refs: Vec<&'b store::Segment> = segments[cursor..cursor + group.len()].to_vec();
        cursor += group.len();
        let mut current: Vec<&'b store::Segment> = Vec::new();
        let mut current_done: Option<bool> = None;
        for segment in group_refs {
            let done = segment.target.is_some();
            if !current.is_empty() && Some(done) != current_done {
                out.push(std::mem::take(&mut current));
            }
            current.push(segment);
            current_done = Some(done);
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

/// Build coarse progress events for a run (jobs integration surface).
pub fn progress_event(
    project_id: &ProjectId,
    run_id: &crate::model::RunId,
    kind: &str,
    done: i64,
    total: i64,
    message: &str,
) -> ProgressEvent {
    ProgressEvent {
        project_id: project_id.clone(),
        run_id: run_id.clone(),
        kind: kind.to_string(),
        done,
        total,
        message: message.to_string(),
    }
}
