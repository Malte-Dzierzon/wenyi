//! Background glossary curation. Oracle: `pipeline/curator.py`.
//!
//! One pass checks up to [`CURATOR_BATCH_SIZE`] terms: collect evidence
//! from the book, ask the cheap tier (`glossary.curate`) whether the
//! mapping holds up, and file disagreements as conflicts. The curator
//! never overwrites established mappings or formal translations.
//!
//! Slice 5 upgrade over the oracle's per-term full-book scan: a
//! [`CandidateIndex`] over the glossary snapshot scans each segment once
//! (verified by oracle boundary semantics), and the
//! `term_occurrences` table caches the result so passes share work.

use std::collections::BTreeMap;
use std::time::Instant;

use rusqlite::Connection;

use crate::glossary::{self, CandidateIndex, GlossaryTerm};
use crate::model::{BookId, ProjectId};
use crate::pipeline_agents::ModelClient;
use crate::{store, Error, Result};

/// Terms checked per curator pass (oracle `CURATOR_BATCH_SIZE`).
pub const CURATOR_BATCH_SIZE: usize = 20;
/// Maximum excerpts attached to one curation prompt.
pub const CURATOR_MAX_EVIDENCE: usize = 5;

/// One place in the project where a term (or an alias) appears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermOccurrence {
    /// Owning book.
    pub book_id: BookId,
    /// Chapter ordinal.
    pub chapter: i64,
    /// Segment ordinal.
    pub segment: i64,
    /// Source text.
    pub source: String,
    /// Translation, when present.
    pub target: Option<String>,
    /// Normalized key that matched.
    pub matched: String,
}

/// One raw scan hit: term source, chapter/segment ordinals, matched key,
/// segment id.
pub type ScanHit = (String, i64, i64, String, String);

/// One index row: term source, book, chapter, match kind, matched key.
type IndexRow = (String, BookId, crate::model::ChapterId, String, String);

/// Pick first/last/middle excerpts (oracle `pick_excerpts`).
pub fn pick_excerpts(matches: Vec<TermOccurrence>, limit: usize) -> Vec<TermOccurrence> {
    if matches.len() <= limit || limit < 2 {
        return matches;
    }
    let mut picked = vec![matches[0].clone()];
    let step = (matches.len() - 1) as f64 / (limit - 1) as f64;
    for slot in 1..limit - 1 {
        picked.push(matches[bankers_round(slot as f64 * step) as usize].clone());
    }
    picked.push(matches[matches.len() - 1].clone());
    picked
}

/// Python `round()` (banker's rounding) for excerpt parity: halves go to
/// the even neighbor, unlike Rust's half-away rounding.
pub fn bankers_round(value: f64) -> i64 {
    let floor = value.floor();
    let frac = value - floor;
    if frac != 0.5 {
        value.round() as i64
    } else if (floor as i64) % 2 == 0 {
        floor as i64
    } else {
        floor as i64 + 1
    }
}

/// Verdict for one term (oracle `check_term` return).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuratorVerdict {
    /// `verified` or `conflict`.
    pub status: String,
    /// Better translation, when proposed.
    pub proposed_target: Option<String>,
    /// One-sentence reason.
    pub reason: String,
}

/// Check glossary terms against project evidence, filing conflicts.
pub struct Curator {
    /// Source language code.
    pub src: String,
    /// Target language code.
    pub tgt: String,
}

impl Curator {
    /// Bind resolved languages.
    pub fn new(src: &str, tgt: &str) -> Self {
        Self {
            src: src.to_string(),
            tgt: tgt.to_string(),
        }
    }

    /// Scan one book for all terms in a single pass per segment.
    ///
    /// Returns `(term_source, ordinal, matched)` hits ordered by
    /// chapter/segment; every hit is verified by oracle boundary rules.
    pub fn scan_book(
        conn: &Connection,
        book_id: &BookId,
        terms: &[GlossaryTerm],
    ) -> Result<Vec<ScanHit>> {
        let mut entries = Vec::new();
        for (index, term) in terms.iter().enumerate() {
            for key in glossary::store::term_match_sources(term) {
                entries.push((index, key));
            }
        }
        let index = CandidateIndex::build(&entries);
        let chapters = store::list_chapters(conn, book_id, 0, i64::MAX)?;
        let mut out = Vec::new();
        for chapter in &chapters {
            for segment in store::list_segments(conn, &chapter.id, 0, i64::MAX)? {
                if segment.source.trim().is_empty() {
                    continue;
                }
                for (owner, matched, _start, _end) in index.find_verified(&segment.source) {
                    out.push((
                        terms[owner].source.clone(),
                        chapter.ordinal,
                        segment.ordinal,
                        matched,
                        segment.id.as_str().to_string(),
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Rebuild the occurrence index for one book (dirty-book reindex).
    pub fn reindex_book(
        conn: &Connection,
        project_id: &ProjectId,
        book_id: &BookId,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<usize> {
        let terms = glossary::all_terms(conn, project_id, source_lang, target_lang)?;
        let hits = Self::scan_book(conn, book_id, &terms)?;
        let chapters = store::list_chapters(conn, book_id, 0, i64::MAX)?;
        let chapter_ids: BTreeMap<i64, _> =
            chapters.into_iter().map(|c| (c.ordinal, c.id)).collect();
        let mut by_segment: BTreeMap<String, Vec<IndexRow>> = BTreeMap::new();
        for (source, chapter_ord, _seg_ord, matched, segment_id) in &hits {
            let Some(chapter_id) = chapter_ids.get(chapter_ord) else {
                continue;
            };
            let match_kind = if terms.iter().any(|t| &t.source == source) {
                // Alias or source? The matched key equals the source key
                // only for the canonical source (aliases differ).
                let term = terms.iter().find(|t| &t.source == source).unwrap();
                let canonical = crate::glossary::normalize_match_key(&term.source);
                if matched == &canonical {
                    "source"
                } else {
                    "alias"
                }
            } else {
                "source"
            };
            by_segment.entry(segment_id.clone()).or_default().push((
                source.clone(),
                book_id.clone(),
                chapter_id.clone(),
                match_kind.to_string(),
                matched.clone(),
            ));
        }
        // Rewrite every touched segment's rows. Segments that lost all hits
        // need clearing too: collect touched + previously indexed.
        let mut touched: Vec<String> = by_segment.keys().cloned().collect();
        // Previously indexed segments for this book come out via a direct
        // query; simpler: clear per segment below covers touched rows, and
        // segments with zero hits keep stale rows unless cleared. Fetch the
        // indexed set for this book first.
        let indexed = Self::indexed_segments(conn, book_id)?;
        for segment_id in &indexed {
            if !by_segment.contains_key(segment_id) {
                touched.push(segment_id.clone());
            }
        }
        for segment_id in &touched {
            let rows = by_segment.get(segment_id).cloned().unwrap_or_default();
            let id = crate::model::SegmentId::new(segment_id.clone())?;
            store::replace_segment_occurrences(
                conn,
                project_id,
                source_lang,
                target_lang,
                &id,
                &rows,
            )?;
        }
        Ok(hits.len())
    }

    /// Segment IDs holding index rows for one book.
    fn indexed_segments(conn: &Connection, book_id: &BookId) -> Result<Vec<String>> {
        let mut stmt = conn
            .prepare("SELECT DISTINCT segment_id FROM term_occurrences WHERE book_id = ?")
            .map_err(|e| crate::Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params![book_id.as_str()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| crate::Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| crate::Error::Store(e.to_string()))?);
        }
        Ok(out)
    }

    /// Load evidence excerpts for one term from the index.
    pub fn occurrences_for_term(
        &self,
        conn: &Connection,
        project_id: &ProjectId,
        source: &str,
    ) -> Result<Vec<TermOccurrence>> {
        let rows =
            store::occurrences_for_term(conn, project_id, &self.src, &self.tgt, source, i64::MAX)?;
        let books = store::list_books(conn, project_id)?;
        let ordinals: BTreeMap<String, (i64, String)> = books
            .into_iter()
            .map(|b| {
                (
                    b.id.as_str().to_string(),
                    (b.ordinal, b.id.as_str().to_string()),
                )
            })
            .collect();
        let mut out = Vec::new();
        for row in rows {
            let chapters = store::list_chapters(conn, &row.book_id, 0, i64::MAX)?;
            let chapter_ord = chapters
                .iter()
                .find(|c| c.id == row.chapter_id)
                .map(|c| c.ordinal)
                .unwrap_or(0);
            let segments = store::list_segments(conn, &row.chapter_id, 0, i64::MAX)?;
            let Some(segment) = segments.into_iter().find(|s| s.id == row.segment_id) else {
                continue;
            };
            let _ = ordinals.get(row.book_id.as_str());
            out.push(TermOccurrence {
                book_id: row.book_id.clone(),
                chapter: chapter_ord,
                segment: segment.ordinal,
                source: segment.source,
                target: segment.target,
                matched: row.matched,
            });
        }
        Ok(pick_excerpts(out, CURATOR_MAX_EVIDENCE))
    }

    /// Ask the curator model to review one term (oracle `check_term`).
    pub fn check_term(
        &self,
        client: &mut dyn ModelClient,
        term: &GlossaryTerm,
        occurrences: &[TermOccurrence],
        book_title: &str,
    ) -> Result<CuratorVerdict> {
        let evidence = occurrences
            .iter()
            .map(|occ| {
                let target = match &occ.target {
                    Some(text) if !text.trim().is_empty() => {
                        format!("  =>  {}", text.chars().take(200).collect::<String>())
                    }
                    _ => "  (untranslated)".to_string(),
                };
                format!(
                    "- ch{} seg{} [{}]: {}{}",
                    occ.chapter,
                    occ.segment,
                    occ.matched,
                    occ.source.chars().take(200).collect::<String>(),
                    target
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "Book: {}\nGlossary term: {} -> {} (type: {}{})\nOccurrences in the book:\n{}\n\n\
             Review this term. Reply with JSON only:\n{{\"status\": \"verified\" | \"conflict\", \
             \"proposed_target\": \"<better translation or null>\", \
             \"reason\": \"<one sentence>\", \"duplicate_of\": \"<other term source or null>\"}}",
            if book_title.is_empty() {
                "(unknown)"
            } else {
                book_title
            },
            term.source,
            term.target,
            term.term_type,
            if term.aliases.is_empty() {
                String::new()
            } else {
                format!(", aliases: {}", term.aliases.join(", "))
            },
            if evidence.is_empty() {
                "(none found)"
            } else {
                &evidence
            },
        );
        let messages = vec![crate::llm::compat::Message {
            role: "user".to_string(),
            content: prompt,
        }];
        let text = client.complete(&messages, "glossary.curate", true, None)?;
        let value: serde_json::Value =
            crate::llm::json::parse_json_loose(&text).unwrap_or_default();
        let obj = match value.as_object() {
            Some(obj) => obj,
            None => {
                return Ok(CuratorVerdict {
                    status: "verified".to_string(),
                    proposed_target: None,
                    reason: "unparseable curator reply".to_string(),
                });
            }
        };
        let status = obj
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("verified");
        Ok(CuratorVerdict {
            status: if status == "conflict" {
                "conflict"
            } else {
                "verified"
            }
            .to_string(),
            proposed_target: obj
                .get("proposed_target")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            reason: obj
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// Check up to `CURATOR_BATCH_SIZE` terms; return pass statistics.
    ///
    /// Reads the stop flag every 5 terms; per-term failures skip with an
    /// event. Conflicts file via `upsert_term` (never overwrite).
    #[allow(clippy::too_many_arguments)] // Mirrors the oracle run_pass shape.
    pub fn run_pass(
        &self,
        conn: &Connection,
        client: &mut dyn ModelClient,
        project_id: &ProjectId,
        terms: &[GlossaryTerm],
        book_title: &str,
        events: &mut Vec<serde_json::Value>,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<BTreeMap<String, i64>> {
        let started = Instant::now();
        let mut checked = 0i64;
        let mut verified = 0i64;
        let mut conflicts = 0i64;
        let mut skipped = 0i64;
        let mut stop_requested = false;
        for (index, term) in terms.iter().take(CURATOR_BATCH_SIZE).enumerate() {
            if index % 5 == 0 {
                let state = store::load_curator_state(conn, project_id, &self.src, &self.tgt)?;
                stop_requested = state
                    .get("stop_requested")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            }
            if stop_requested || is_cancelled() {
                events.push(serde_json::json!({
                    "event": "curator_pass_stopped",
                    "checked": checked,
                }));
                break;
            }
            let term_started = Instant::now();
            let indexed = store::count_occurrences_for_term(
                conn,
                project_id,
                &self.src,
                &self.tgt,
                &term.source,
            )?;
            glossary::set_mentions(
                conn,
                project_id,
                &self.src,
                &self.tgt,
                &term.source,
                indexed,
            )?;
            let occurrences = self.occurrences_for_term(conn, project_id, &term.source)?;
            let verdict = match self.check_term(client, term, &occurrences, book_title) {
                Ok(verdict) => verdict,
                Err(error) => {
                    skipped += 1;
                    events.push(serde_json::json!({
                        "event": "curator_term_skipped",
                        "source": term.source,
                        "error_type": error_kind(&error),
                        "reason": error.to_string().chars().take(200).collect::<String>(),
                        "elapsed_seconds": round1(term_started.elapsed()),
                    }));
                    continue;
                }
            };
            checked += 1;
            if verdict.status == "conflict" {
                conflicts += 1;
                let proposed = verdict
                    .proposed_target
                    .clone()
                    .unwrap_or_else(|| term.target.clone());
                let mut candidate = GlossaryTerm::new(term.source.clone(), proposed);
                candidate.term_type = term.term_type.clone();
                candidate.note = format!("curator: {}", verdict.reason);
                glossary::upsert_term(
                    conn,
                    project_id,
                    &self.src,
                    &self.tgt,
                    &candidate,
                    term.first_chapter,
                )?;
            } else {
                verified += 1;
            }
            events.push(serde_json::json!({
                "event": "curator_term_checked",
                "source": term.source,
                "status": verdict.status,
                "occurrences": occurrences.len(),
                "reason": verdict.reason,
                "proposed_target": verdict.proposed_target,
                "elapsed_seconds": round1(term_started.elapsed()),
            }));
        }
        events.push(serde_json::json!({
            "event": "curator_pass_finished",
            "checked": checked,
            "verified": verified,
            "conflicts": conflicts,
            "skipped": skipped,
            "elapsed_seconds": round1(started.elapsed()),
        }));
        Ok(BTreeMap::from([
            ("checked".to_string(), checked),
            ("verified".to_string(), verified),
            ("conflicts".to_string(), conflicts),
            ("skipped".to_string(), skipped),
        ]))
    }

    /// Select the next terms: open conflicts first, then least-recently
    /// checked (oracle `run_curator` ordering).
    pub fn select_terms(
        conn: &Connection,
        project_id: &ProjectId,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<Vec<GlossaryTerm>> {
        let terms = glossary::all_terms(conn, project_id, source_lang, target_lang)?;
        let conflicts = glossary::open_conflicts(conn, project_id, source_lang, target_lang)?;
        let conflicted: std::collections::BTreeSet<String> = conflicts
            .into_iter()
            .filter_map(|row| {
                row.get("source")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();
        let state = store::load_curator_state(conn, project_id, source_lang, target_lang)?;
        let checked_at = state
            .get("checked_at")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let mut ranked: Vec<(bool, String, usize)> = terms
            .iter()
            .enumerate()
            .map(|(index, term)| {
                (
                    !conflicted.contains(&term.source),
                    checked_at
                        .get(&term.source)
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    index,
                )
            })
            .collect();
        ranked.sort();
        Ok(ranked
            .into_iter()
            .take(CURATOR_BATCH_SIZE)
            .map(|(_, _, index)| terms[index].clone())
            .collect())
    }

    /// Stamp `checked_at` for the pass terms + `last_pass` stats.
    pub fn record_pass(
        conn: &Connection,
        project_id: &ProjectId,
        source_lang: &str,
        target_lang: &str,
        terms: &[GlossaryTerm],
        stats: &BTreeMap<String, i64>,
    ) -> Result<()> {
        let mut state = store::load_curator_state(conn, project_id, source_lang, target_lang)?;
        state.remove("stop_requested");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
            .to_string();
        let mut checked_at = state
            .get("checked_at")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        for term in terms {
            checked_at.insert(term.source.clone(), serde_json::Value::String(now.clone()));
        }
        state.insert(
            "checked_at".to_string(),
            serde_json::Value::Object(checked_at),
        );
        let last: BTreeMap<String, serde_json::Value> = stats
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::Number((*v).into())))
            .collect();
        state.insert(
            "last_pass".to_string(),
            serde_json::Value::Object(last.into_iter().collect()),
        );
        store::save_curator_state(
            conn,
            project_id,
            source_lang,
            target_lang,
            &serde_json::Value::Object(state).to_string(),
        )
    }

    /// Build the `run_glossary_curator` jobs body.
    ///
    /// Reads `source_lang`/`target_lang` from params (defaulting to the
    /// curator's languages), selects terms, runs one pass, records it.
    /// Events land in `job_events`; cancel maps to `Interrupted`.
    pub fn task(
        curator: Curator,
        make_client: impl Fn() -> Box<dyn ModelClient> + Send + Sync + 'static,
    ) -> crate::jobs::TaskFn {
        use std::sync::Arc;
        let make_client = Arc::new(make_client);
        Arc::new(
            move |work: crate::jobs::WorkItem, cancel: crate::jobs::Cancel| {
                let make_client = Arc::clone(&make_client);
                let curator = Curator::new(&curator.src, &curator.tgt);
                Box::pin(async move {
                    let run = || -> Result<crate::jobs::TaskOutcome> {
                        let db_path = work
                            .params
                            .get("db_path")
                            .and_then(|v| v.as_str())
                            .map(std::path::PathBuf::from);
                        // The runtime owns the DB path; params carry the pair.
                        let Some(db_path) = db_path else {
                            return Ok(crate::jobs::TaskOutcome::Failed(
                                "The curator task is missing its database path.".to_string(),
                            ));
                        };
                        let conn = store::open(&db_path)?;
                        let src = work
                            .params
                            .get("source_lang")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&curator.src)
                            .to_string();
                        let tgt = work
                            .params
                            .get("target_lang")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&curator.tgt)
                            .to_string();
                        let terms = Curator::select_terms(&conn, &work.project_id, &src, &tgt)?;
                        // Reindex dirty books first so the pass sees fresh
                        // evidence (cheap: single scan per book over matches).
                        for book in store::list_books(&conn, &work.project_id)? {
                            Curator::reindex_book(&conn, &work.project_id, &book.id, &src, &tgt)?;
                        }
                        let mut client = make_client();
                        let mut events = Vec::new();
                        let pass = Curator::new(&src, &tgt);
                        let stats = pass.run_pass(
                            &conn,
                            client.as_mut(),
                            &work.project_id,
                            &terms,
                            "",
                            &mut events,
                            &|| cancel.is_cancelled(),
                        )?;
                        Curator::record_pass(&conn, &work.project_id, &src, &tgt, &terms, &stats)?;
                        for event in &events {
                            let kind = event
                                .get("event")
                                .and_then(|v| v.as_str())
                                .unwrap_or("curator_event");
                            let _ = store::append_job_event(
                                &conn,
                                &work.job_id,
                                kind,
                                &event.to_string(),
                            );
                        }
                        if cancel.is_cancelled() {
                            return Ok(crate::jobs::TaskOutcome::Interrupted(
                                "The curator pass was cancelled.".to_string(),
                            ));
                        }
                        Ok(crate::jobs::TaskOutcome::Done)
                    };
                    match run() {
                        Ok(outcome) => outcome,
                        Err(error) => crate::jobs::TaskOutcome::Failed(error.to_string()),
                    }
                }) as crate::jobs::futures::TaskFuture
            },
        )
    }
}

/// Short error kind for skip events (stable strings, no internals).
fn error_kind(error: &Error) -> &'static str {
    match error {
        Error::Config(_) => "Config",
        Error::Store(_) => "Store",
        Error::Io { .. } => "Io",
        Error::NotFound { .. } => "NotFound",
        Error::Provider(_) => "Provider",
        Error::Stopped(_) => "Stopped",
        Error::Conflict(_) => "Conflict",
        Error::Unsupported(_) => "Unsupported",
    }
}

/// Elapsed seconds rounded to 0.1 (oracle `round(..., 1)`).
fn round1(elapsed: std::time::Duration) -> f64 {
    (elapsed.as_secs_f64() * 10.0).round() / 10.0
}
