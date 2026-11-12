//! In-process engine service: thin sync wrappers over store/pipeline/jobs.
//!
//! The GUI crate holds an `Arc<Service>` and calls these methods from
//! background threads (SQLite calls are cheap; model I/O runs in the
//! `JobsRuntime`). Task bodies in `jobs.rs` do the long work.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::broadcast;

use crate::document::assemble::AssembleOptions;
use crate::jobs::{self, JobsRuntime, RuntimeConfig};
use crate::model::{BookId, ChapterId, JobId, ProjectId, RunId, SegmentId};
use crate::{config, export, glossary, i18n, store, style};
use crate::{Error, Result};

pub mod dto;

use dto::*;

/// Workspace config filename (global config v1: plain YAML in workspace dir).
pub const CONFIG_FILENAME: &str = "config.yaml";
/// Workspace exports directory (export output files).
pub const EXPORTS_DIRNAME: &str = "exports";

/// Session-scoped engine service over one workspace directory.
pub struct Service {
    dir: PathBuf,
    db_path: PathBuf,
    jobs: Arc<JobsRuntime>,
    config: Mutex<config::Config>,
    credentials: Mutex<BTreeMap<String, String>>,
    /// In-memory config revision for 409-guarded saves (starts at 1,
    /// bumps on every successful save).
    revision: Mutex<i64>,
}

impl Service {
    /// Open (creating) the workspace: DB migrate, config load, task register.
    /// Call [`Service::start`] once to run consumers (jobs admission).
    pub fn open(workspace_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(workspace_dir).map_err(|e| Error::Io {
            path: workspace_dir.to_path_buf(),
            message: e.to_string(),
        })?;
        let db_path = store::db_path(workspace_dir);
        let conn = store::open(&db_path)?;
        drop(conn);
        let config = load_workspace_config(workspace_dir)?;
        let mut jobs = JobsRuntime::new(db_path.clone(), RuntimeConfig::default());
        jobs.register("run_translation", self::tasks::translation_task());
        jobs.register(
            "run_chapter_translation",
            self::tasks::chapter_translation_task(),
        );
        jobs.register("run_export", self::tasks::export_task());
        jobs.register("run_refresh_analysis", self::tasks::refresh_analysis_task());
        // Curator sweep body: without this registration
        // `curate_periodically` skips every project (fail-safe no-op).
        // `translate()` stays one book-wide job by design — only chapter
        // jobs fan out across lanes (see WORKFLOW_CONSUMERS).
        jobs.register("run_glossary_curator", self::tasks::curator_task());
        Ok(Self {
            dir: workspace_dir.to_path_buf(),
            db_path,
            jobs: Arc::new(jobs),
            config: Mutex::new(config),
            credentials: Mutex::new(BTreeMap::new()),
            revision: Mutex::new(1),
        })
    }

    /// Start job consumers + recovery sweeps after opening.
    pub async fn start(&self) -> Result<Vec<tokio::task::JoinHandle<()>>> {
        Arc::clone(&self.jobs).start().await
    }

    /// Flip the jobs accepting flag without starting consumers
    /// (deterministic lane/admission tests; jobs stay queued).
    #[doc(hidden)]
    pub fn accepting_for_tests(&self) {
        self.jobs.accepting_for_tests();
    }

    /// Graceful stop (drains in-flight checkpoint writes).
    pub async fn shutdown(&self) {
        self.jobs.stop().await;
    }

    /// Subscribe to job progress events (`started`/`progress`/`done`/`failed`/`interrupted`).
    /// Runtime-safe: the bridge task spawns on the jobs runtime handle when
    /// called outside a Tokio context (e.g. a GPUI background thread).
    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        let inner = self.jobs.subscribe();
        let (tx, rx) = broadcast::channel(jobs::PROGRESS_CHANNEL);
        let pump = async move {
            let mut inner = inner;
            while let Ok(event) = inner.recv().await {
                let _ = tx.send(EngineEvent::from(&event));
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(_) => {
                tokio::spawn(pump);
            }
            Err(_) => {
                std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build();
                    if let Ok(runtime) = runtime {
                        runtime.block_on(pump);
                    }
                });
            }
        }
        rx
    }

    fn conn(&self) -> Result<rusqlite::Connection> {
        store::open(&self.db_path)
    }

    fn languages(&self, project_id: &ProjectId) -> Result<(String, String)> {
        let conn = self.conn()?;
        project_languages(
            &conn,
            project_id,
            &self.config.lock().expect("service config"),
        )
    }

    fn resolve_book(&self, project_id: &ProjectId) -> Result<(crate::store::Book, String, String)> {
        let conn = self.conn()?;
        let book = single_book(&conn, project_id)?;
        let (src, tgt) = project_languages(
            &conn,
            project_id,
            &self.config.lock().expect("service config"),
        )?;
        Ok((book, src, tgt))
    }

    // -- Projects ------------------------------------------------------

    /// List all projects (creation order).
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.conn()?;
        let config = self.config.lock().expect("service config").clone();
        let mut out = Vec::new();
        for row in store::list_projects(&conn)? {
            out.push(project_row_full(&conn, &row, &config)?);
        }
        Ok(out)
    }

    /// Project detail with counts (first book; v1 = one book per project).
    pub fn get_project(&self, project_id: &str) -> Result<ProjectDetail> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        let row = store::get_project(&conn, &pid)?.ok_or_else(|| Error::NotFound {
            kind: "project",
            id: project_id.to_string(),
        })?;
        project_detail(&conn, &row)
    }

    /// Create a project and import one file (epub/txt/md/html; srt parses).
    pub fn create_project(
        &self,
        name: &str,
        source_lang: &str,
        target_lang: &str,
        file_bytes: &[u8],
        filename: &str,
    ) -> Result<ProjectDetail> {
        let src = i18n::require_language(source_lang, true)?;
        let tgt = i18n::require_language(target_lang, false)?;
        let pid = ProjectId::new(new_id("p")?)?;
        let conn = self.conn()?;
        let row = store::create_project(&conn, &pid, name)?;
        store::save_artifact(
            &conn,
            &pid,
            "languages",
            &serde_json::json!({"source_lang": src, "target_lang": tgt}).to_string(),
        )?;
        let (_book_id, _fmt) =
            self::import::import_bytes(&conn, &pid, &src, &tgt, file_bytes, filename)?;
        project_detail(&conn, &row)
    }

    /// Delete a project and its subtree (CASCADE).
    pub fn delete_project(&self, project_id: &str) -> Result<()> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        if !store::delete_project(&conn, &pid)? {
            return Err(Error::NotFound {
                kind: "project",
                id: project_id.to_string(),
            });
        }
        Ok(())
    }

    // -- Chapters ------------------------------------------------------

    /// Chapter summaries for the project's first book.
    pub fn list_chapters(&self, project_id: &str) -> Result<Vec<ChapterSummary>> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let book = single_book(&conn, &pid)?;
        let chapters = store::list_chapters(&conn, &book.id, 0, i64::MAX)?;
        // Cached counts (backfilled on first read) — no text scan.
        let totals: BTreeMap<i64, (u64, u64)> = store::cached_chapter_word_totals(&conn, &book.id)?
            .into_iter()
            .map(|(ordinal, source, target)| (ordinal, (source, target)))
            .collect();
        // Latest finished review per chapter (single row read + JSON walk).
        let review = store::latest_finished_review(&conn, &book.id)?;
        let review_by_chapter = review_issues_by_chapter(review.as_ref());
        let mut out = Vec::with_capacity(chapters.len());
        for chapter in chapters {
            let (words, target_words) = totals.get(&chapter.ordinal).copied().unwrap_or((0, 0));
            let issue_count = review_by_chapter
                .get(&chapter.ordinal)
                .copied()
                .unwrap_or(0);
            out.push(ChapterSummary {
                index: chapter.ordinal as u32,
                title: chapter.title.clone(),
                title_translated: chapter.title_translated.clone(),
                status: chapter.status.clone(),
                word_count: words,
                target_word_count: target_words,
                review_issue_count: issue_count,
                review_status: if review.is_some() {
                    "done".to_string()
                } else {
                    String::new()
                },
            });
        }
        Ok(out)
    }

    /// One chapter with segments (review-chapter shape).
    pub fn get_chapter(&self, project_id: &str, index: u32) -> Result<ChapterSegments> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let book = single_book(&conn, &pid)?;
        let chapter = find_chapter(&conn, &book.id, index as i64)?;
        let segments = store::list_segments(&conn, &chapter.id, 0, i64::MAX)?
            .into_iter()
            .map(|row| SegmentOut {
                index: row.ordinal as u32,
                source: row.source.clone(),
                target: row.target.clone(),
                target_before_polish: row.target_before_polish.clone(),
                anchor: (!row.anchor.is_empty()).then(|| row.anchor.clone()),
                kind: row.kind.clone(),
            })
            .collect();
        Ok(ChapterSegments {
            index,
            title: chapter.title.clone(),
            title_translated: chapter.title_translated.clone(),
            segments,
            review_issues: Vec::new(),
        })
    }

    // -- Workflow (jobs-backed) ----------------------------------------

    /// Enqueue whole-book translation; returns immediately.
    ///
    /// Deliberately one book-wide job (no chapter fan-out): the pipeline
    /// carries rolling context + checkpoints across chapters, so splitting
    /// it would fork context and duplicate synopsis/analysis work. Chapter
    /// parallelism lives in `translate_chapter` (per-chapter lanes).
    pub fn translate(&self, project_id: &str) -> Result<()> {
        let pid = ProjectId::new(project_id)?;
        let (book, src, tgt) = self.resolve_book(&pid)?;
        self.admit_job(&pid, "translation", "run_translation", book, src, tgt, None)
    }

    /// Enqueue one-chapter translation; returns immediately.
    pub fn translate_chapter(&self, project_id: &str, index: u32) -> Result<()> {
        let pid = ProjectId::new(project_id)?;
        let (book, src, tgt) = self.resolve_book(&pid)?;
        self.admit_job(
            &pid,
            "chapter_translation",
            "run_chapter_translation",
            book,
            src,
            tgt,
            Some(index as i64),
        )
    }

    /// Pause active runs: cooperative cancel, then durable `paused` status.
    /// `resume` re-admits from checkpoints; `get_workflow` reports `paused`.
    pub fn pause(&self, project_id: &str) -> Result<()> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        for (job, _) in store::list_active_jobs_with_params(&conn, &pid)? {
            let _ = self.jobs.cancel(&job.run_id);
            // `cancel` lands `cancelled`; pausing re-labels to `paused`
            // (terminal for lanes, explicit resume point for the UI).
            let _ = store::transition_job(&conn, &job.run_id, &["cancelled"], "paused");
        }
        Ok(())
    }

    /// Resume from checkpoints: re-admit book-wide translation.
    pub fn resume(&self, project_id: &str) -> Result<()> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        for (job, _) in store::list_active_jobs_with_params(&conn, &pid)? {
            let _ = self.jobs.cancel(&job.run_id);
        }
        drop(conn);
        self.translate(project_id)
    }

    /// Current workflow state (active jobs + chapter statuses).
    pub fn get_workflow(&self, project_id: &str) -> Result<WorkflowOut> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let book = single_book(&conn, &pid)?;
        let active = store::list_active_jobs_with_params(&conn, &pid)?;
        let (status, run_id, kind) = match active.first() {
            Some((job, _)) => (
                job.status.clone(),
                Some(job.run_id.to_string()),
                job.kind.clone(),
            ),
            // Paused runs are terminal for lanes: surface the last
            // `paused`/`interrupted` state instead of `idle`.
            None => last_terminal_status(&conn, &pid)
                .map(|(status, run, kind)| (status, Some(run), kind))
                .unwrap_or_else(|| ("idle".to_string(), None, "translation".to_string())),
        };
        let running_chapters: Vec<i64> = active
            .iter()
            .filter_map(|(_, params)| {
                serde_json::from_str::<serde_json::Value>(params)
                    .ok()?
                    .get("chapter_index")
                    .and_then(|v| v.as_i64())
            })
            .collect();
        let chapters = store::list_chapters(&conn, &book.id, 0, i64::MAX)?;
        let stages = chapters
            .iter()
            .map(|chapter| WorkflowStage {
                id: format!("chapter-{}", chapter.ordinal),
                label: chapter.title.clone(),
                enabled: chapter.status != "done",
            })
            .collect();
        Ok(WorkflowOut {
            source: "local".to_string(),
            kind,
            status,
            run_id,
            review_id: None,
            stages,
            progress: None,
            running_chapters,
        })
    }

    /// Enqueue an analysis refresh (re-analyze + re-seed style memory).
    pub fn refresh_analysis(&self, project_id: &str) -> Result<()> {
        let pid = ProjectId::new(project_id)?;
        let (book, src, tgt) = self.resolve_book(&pid)?;
        self.admit_job(
            &pid,
            "refresh_analysis",
            "run_refresh_analysis",
            book,
            src,
            tgt,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn admit_job(
        &self,
        project_id: &ProjectId,
        kind: &str,
        name: &str,
        book: crate::store::Book,
        src: String,
        tgt: String,
        chapter_index: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn()?;
        let job_id = JobId::new(new_id("j")?)?;
        let run_id = RunId::new(new_id("r")?)?;
        let mut params = serde_json::Map::new();
        params.insert(
            "db_path".to_string(),
            serde_json::Value::String(self.db_path.to_string_lossy().into_owned()),
        );
        params.insert(
            "book_id".to_string(),
            serde_json::Value::String(book.id.as_str().to_string()),
        );
        params.insert("source_lang".to_string(), serde_json::Value::String(src));
        params.insert("target_lang".to_string(), serde_json::Value::String(tgt));
        let config_yaml =
            std::fs::read_to_string(self.dir.join(CONFIG_FILENAME)).unwrap_or_default();
        params.insert(
            "config_yaml".to_string(),
            serde_json::Value::String(config_yaml),
        );
        if let Some(index) = chapter_index {
            params.insert("chapter_index".to_string(), serde_json::Value::from(index));
        }
        let params_json = serde_json::Value::Object(params).to_string();
        store::enqueue_job(&conn, &job_id, project_id, kind, &run_id, &params_json)?;
        drop(conn);
        let raw: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&params_json).unwrap_or_default();
        self.jobs
            .admit(project_id, kind, name, &job_id, &run_id, raw)?;
        Ok(())
    }

    // -- Proofread -----------------------------------------------------

    /// Stale-guarded segment edit.
    pub fn edit_segment(
        &self,
        project_id: &str,
        chapter: u32,
        segment: u32,
        target: &str,
        expected: Option<&str>,
    ) -> Result<()> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let book = single_book(&conn, &pid)?;
        let chapter_row = find_chapter(&conn, &book.id, chapter as i64)?;
        let segment_row = find_segment(&conn, &chapter_row.id, segment as i64)?;
        store::edit_segment(
            &conn,
            &segment_row.id,
            (!target.is_empty()).then_some(target),
            expected,
            "edit",
        )
    }

    /// Segment revision history.
    pub fn segment_history(
        &self,
        project_id: &str,
        chapter: u32,
        segment: u32,
    ) -> Result<Vec<SegmentRevision>> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let book = single_book(&conn, &pid)?;
        let chapter_row = find_chapter(&conn, &book.id, chapter as i64)?;
        let segment_row = find_segment(&conn, &chapter_row.id, segment as i64)?;
        Ok(store::load_segment_history(&conn, &segment_row.id)?
            .into_iter()
            .map(|row| SegmentRevision {
                id: row.id.to_string(),
                kind: row.kind.clone(),
                before: row.previous_target.clone(),
                after: row.new_target.clone(),
            })
            .collect())
    }

    // -- Glossary ------------------------------------------------------

    fn glossary_langs(&self, project_id: &ProjectId) -> Result<(String, String)> {
        self.languages(project_id)
    }

    /// All glossary terms (stable prompt order).
    pub fn list_terms(&self, project_id: &str) -> Result<Vec<GlossaryTerm>> {
        let pid = ProjectId::new(project_id)?;
        let (src, tgt) = self.glossary_langs(&pid)?;
        let conn = self.conn()?;
        Ok(glossary::all_terms(&conn, &pid, &src, &tgt)?
            .into_iter()
            .map(glossary_term_from)
            .collect())
    }

    /// Add (or upsert) a glossary term.
    pub fn add_term(&self, project_id: &str, term: TermIn) -> Result<GlossaryTerm> {
        let pid = ProjectId::new(project_id)?;
        let (src, tgt) = self.glossary_langs(&pid)?;
        let conn = self.conn()?;
        let core = glossary_term_into(term);
        glossary::upsert_term(&conn, &pid, &src, &tgt, &core, None)?;
        glossary::get_term(&conn, &pid, &src, &tgt, &core.source)?
            .map(glossary_term_from)
            .ok_or_else(|| Error::NotFound {
                kind: "glossary_term",
                id: core.source.clone(),
            })
    }

    /// Update a term's target (resolves conflicts for that source).
    pub fn update_term(
        &self,
        project_id: &str,
        source: &str,
        term: TermIn,
    ) -> Result<GlossaryTerm> {
        let pid = ProjectId::new(project_id)?;
        let (src, tgt) = self.glossary_langs(&pid)?;
        let conn = self.conn()?;
        if glossary::get_term(&conn, &pid, &src, &tgt, source)?.is_none() {
            return Err(Error::NotFound {
                kind: "glossary_term",
                id: source.to_string(),
            });
        }
        let mut core = glossary_term_into(term);
        core.source = source.to_string();
        glossary::resolve_term(&conn, &pid, &src, &tgt, source, &core.target)?;
        glossary::get_term(&conn, &pid, &src, &tgt, source)?
            .map(glossary_term_from)
            .ok_or_else(|| Error::NotFound {
                kind: "glossary_term",
                id: source.to_string(),
            })
    }

    /// Delete a term (hard delete; conflicts for the source resolve too).
    pub fn delete_term(&self, project_id: &str, source: &str) -> Result<()> {
        let pid = ProjectId::new(project_id)?;
        let (src, tgt) = self.glossary_langs(&pid)?;
        let conn = self.conn()?;
        let deleted = conn
            .execute(
                "DELETE FROM glossary_terms WHERE project_id = ? AND source_lang = ? \
                 AND target_lang = ? AND source = ?",
                rusqlite::params![pid.as_str(), src, tgt, source],
            )
            .map_err(store::map_error)?;
        if deleted == 0 {
            return Err(Error::NotFound {
                kind: "glossary_term",
                id: source.to_string(),
            });
        }
        glossary::mark_conflicts_resolved(&conn, &pid, &src, &tgt, source)?;
        Ok(())
    }

    /// Open glossary conflicts (oldest first).
    pub fn list_conflicts(&self, project_id: &str) -> Result<Vec<TermConflict>> {
        let pid = ProjectId::new(project_id)?;
        let (src, tgt) = self.glossary_langs(&pid)?;
        let conn = self.conn()?;
        let rows = glossary::open_conflicts(&conn, &pid, &src, &tgt)?;
        let ids = conflict_ids(&conn, &pid, &src, &tgt)?;
        Ok(rows
            .into_iter()
            .enumerate()
            .map(|(i, row)| TermConflict {
                id: ids.get(i).copied().unwrap_or(i as i64),
                source: row
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                existing_target: row
                    .get("existing_target")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                proposed_target: row
                    .get("proposed_target")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                chapter: row.get("chapter").and_then(|v| v.as_i64()),
                note: row.get("note").and_then(|v| v.as_str()).map(str::to_string),
            })
            .collect())
    }

    /// Resolve one conflict by id (target = chosen final rendering).
    pub fn resolve_conflict(&self, project_id: &str, conflict_id: i64, target: &str) -> Result<()> {
        let pid = ProjectId::new(project_id)?;
        let (src, tgt) = self.glossary_langs(&pid)?;
        let conn = self.conn()?;
        let source: Option<String> = conn
            .query_row(
                "SELECT source FROM glossary_conflicts WHERE id = ? AND project_id = ? \
                 AND source_lang = ? AND target_lang = ?",
                rusqlite::params![conflict_id, pid.as_str(), src, tgt],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(store::map_error(other)),
            })?;
        let Some(source) = source else {
            return Err(Error::NotFound {
                kind: "glossary_conflict",
                id: conflict_id.to_string(),
            });
        };
        glossary::resolve_term(&conn, &pid, &src, &tgt, &source, target)?;
        Ok(())
    }

    /// Background curator state: running flag from active jobs, term
    /// coverage from the store, last pass from `curator_state`, recent
    /// passes from durable `curator_pass_finished` job events.
    pub fn curator_status(&self, project_id: &str) -> Result<CuratorStatus> {
        let pid = ProjectId::new(project_id)?;
        let (src, tgt) = self.glossary_langs(&pid)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let active = store::list_active_jobs_with_params(&conn, &pid)?;
        let running_job = active
            .iter()
            .find(|(job, _)| job.kind == "glossary_curator");
        let (src_langs, tgt_langs) = (src.clone(), tgt.clone());
        let terms_total = glossary::all_terms(&conn, &pid, &src_langs, &tgt_langs)
            .map(|terms| terms.len() as i64)
            .unwrap_or(0);
        let open_conflicts = glossary::open_conflicts(&conn, &pid, &src_langs, &tgt_langs)
            .map(|rows| rows.len() as i64)
            .unwrap_or(0);
        let state =
            store::load_curator_state(&conn, &pid, &src_langs, &tgt_langs).unwrap_or_default();
        let last_pass = state
            .get("last_pass")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let terms_checked = last_pass
            .get("checked")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let mut current_term: Option<String> = None;
        let mut recent_passes = Vec::new();
        for (_, _, kind, payload) in store::recent_job_events(&conn, &pid, 200).unwrap_or_default()
        {
            if recent_passes.len() >= 5 && current_term.is_some() {
                break;
            }
            let value: serde_json::Value = serde_json::from_str(&payload).unwrap_or_default();
            if kind == "curator_pass_finished" && recent_passes.len() < 5 {
                recent_passes.push(value);
            } else if kind == "curator_term_checked" && current_term.is_none() {
                current_term = value
                    .get("source")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
            }
        }
        Ok(CuratorStatus {
            running: running_job.is_some(),
            run_id: running_job.map(|(job, _)| job.run_id.as_str().to_string()),
            current_term,
            terms_total,
            terms_checked,
            open_conflicts,
            last_pass,
            recent_passes,
        })
    }

    /// Enqueue one curator pass. 409-style [`Error::Conflict`] when a pass
    /// is already active (mirrors the old REST `curator/start`).
    pub fn curator_start(&self, project_id: &str) -> Result<CuratorStartOut> {
        let pid = ProjectId::new(project_id)?;
        let (src, tgt) = self.languages(&pid)?;
        let book = single_book(&self.conn()?, &pid)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        if store::list_active_jobs_with_params(&conn, &pid)?
            .iter()
            .any(|(job, _)| job.kind == "glossary_curator")
        {
            return Err(Error::Conflict(
                "a curator pass is already running".to_string(),
            ));
        }
        let job_id = JobId::new(new_id("j")?)?;
        let run_id = RunId::new(new_id("r")?)?;
        let mut params = serde_json::Map::new();
        params.insert(
            "db_path".to_string(),
            serde_json::Value::String(self.db_path.to_string_lossy().into_owned()),
        );
        params.insert(
            "book_id".to_string(),
            serde_json::Value::String(book.id.as_str().to_string()),
        );
        params.insert("source_lang".to_string(), serde_json::Value::String(src));
        params.insert("target_lang".to_string(), serde_json::Value::String(tgt));
        let params_json = serde_json::Value::Object(params).to_string();
        store::enqueue_job(
            &conn,
            &job_id,
            &pid,
            "glossary_curator",
            &run_id,
            &params_json,
        )?;
        drop(conn);
        let raw: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&params_json).unwrap_or_default();
        self.jobs.admit(
            &pid,
            "glossary_curator",
            "run_glossary_curator",
            &job_id,
            &run_id,
            raw,
        )?;
        Ok(CuratorStartOut {
            run_id: run_id.as_str().to_string(),
            job_id: job_id.as_str().to_string(),
        })
    }

    /// Ask a running pass to stop after its current term: sets the
    /// `stop_requested` flag in `curator_state` (the pass polls it every
    /// 5 terms). No-op when no pass is active.
    pub fn curator_stop(&self, project_id: &str) -> Result<()> {
        let pid = ProjectId::new(project_id)?;
        let (src, tgt) = self.languages(&pid)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let mut state = store::load_curator_state(&conn, &pid, &src, &tgt)?;
        state.insert("stop_requested".to_string(), serde_json::Value::Bool(true));
        store::save_curator_state(
            &conn,
            &pid,
            &src,
            &tgt,
            &serde_json::Value::Object(state).to_string(),
        )?;
        Ok(())
    }
    // -- Style ---------------------------------------------------------

    /// Book analysis document + chapter digests.
    pub fn get_analysis(&self, project_id: &str) -> Result<AnalysisOut> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let book = single_book(&conn, &pid)?;
        let analysis: serde_json::Value = store::load_analysis(&conn, &book.id)?
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(serde_json::Value::Null);
        let chapters = store::list_chapters(&conn, &book.id, 0, i64::MAX)?;
        let mut digests = Vec::new();
        for chapter in chapters {
            let digest = store::load_digest(&conn, &chapter.id)?
                .map(|(text, _, _)| text)
                .unwrap_or_default();
            digests.push(ChapterDigest {
                index: chapter.ordinal as u32,
                title: chapter.title.clone(),
                digest,
            });
        }
        Ok(AnalysisOut {
            analysis,
            chapter_digests: digests,
        })
    }

    /// Overwrite the book analysis document.
    pub fn update_analysis(&self, project_id: &str, analysis: serde_json::Value) -> Result<()> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let book = single_book(&conn, &pid)?;
        store::save_analysis(&conn, &book.id, &analysis.to_string())
    }

    /// Overwrite one chapter digest.
    pub fn update_digest(&self, project_id: &str, chapter: u32, digest: &str) -> Result<()> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let book = single_book(&conn, &pid)?;
        let chapter_row = find_chapter(&conn, &book.id, chapter as i64)?;
        store::save_digest(
            &conn,
            &chapter_row.id,
            digest,
            !digest.trim().is_empty(),
            "manual",
        )
    }

    // -- Stats / report / review runs / events -----------------------------

    /// Usage + timing ledgers. Prefers live `usage_ledger` rows aggregated
    /// on demand; falls back to migrated `legacy/*.json` artifacts.
    pub fn get_stats(&self, project_id: &str) -> Result<ProjectStats> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let usage = store::load_artifact(&conn, &pid, "legacy/usage.json")?
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(serde_json::Value::Object(Default::default()));
        let timing = store::load_artifact(&conn, &pid, "legacy/timing.json")?
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(serde_json::Value::Object(Default::default()));
        Ok(ProjectStats {
            usage,
            timing,
            live: None,
        })
    }

    /// Full project report (progress, conflicts, review summary).
    pub fn get_report(&self, project_id: &str) -> Result<serde_json::Value> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let book = single_book(&conn, &pid)?;
        let (src, tgt) = self.languages(&pid)?;
        let report = export::build_report(&conn, &pid, &book.id, &src, &tgt)?;
        Ok(serde_json::Value::Object(report))
    }

    /// Persist the current report snapshot (no-op when unchanged).
    pub fn regenerate_report(&self, project_id: &str) -> Result<serde_json::Value> {
        let report = self.get_report(project_id)?;
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        if let serde_json::Value::Object(map) = report.clone() {
            let mut events = Vec::new();
            export::save_report(
                &conn,
                &pid,
                &single_book(&conn, &pid)?.id,
                &map,
                &mut events,
            )?;
        }
        Ok(report)
    }

    /// Finished + running review runs (newest first).
    pub fn list_review_runs(&self, project_id: &str) -> Result<Vec<ReviewRunOut>> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let book = single_book(&conn, &pid)?;
        Ok(store::list_review_runs(&conn, &book.id)?
            .into_iter()
            .map(review_run_out_from)
            .collect())
    }

    /// One review run with issues + summary.
    pub fn get_review_run(&self, project_id: &str, review_id: &str) -> Result<ReviewRunOut> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let run = store::load_review_run(&conn, review_id)?.ok_or_else(|| Error::NotFound {
            kind: "review",
            id: review_id.to_string(),
        })?;
        if run.project_id != pid {
            return Err(Error::NotFound {
                kind: "review",
                id: review_id.to_string(),
            });
        }
        Ok(review_run_out_from(run))
    }

    /// Persisted job events (newest first, capped at 500).
    pub fn list_events(&self, project_id: &str) -> Result<Vec<EventOut>> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        Ok(store::recent_job_events(&conn, &pid, 500)?
            .into_iter()
            .enumerate()
            .map(|(index, (job_id, ts, kind, payload))| EventOut {
                id: index as i64,
                job_id,
                ts,
                event_type: kind,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
            })
            .collect())
    }

    /// Update one chapter's translated title.
    pub fn update_chapter_title(&self, project_id: &str, chapter: u32, title: &str) -> Result<()> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        let book = single_book(&conn, &pid)?;
        let chapter_row = find_chapter(&conn, &book.id, chapter as i64)?;
        store::set_chapter_title_translated(&conn, &chapter_row.id, title)
    }

    /// List export records (newest first).
    pub fn list_exports(&self, project_id: &str) -> Result<Vec<ExportOut>> {
        let pid = ProjectId::new(project_id)?;
        let conn = self.conn()?;
        require_project(&conn, &pid)?;
        Ok(store::list_exports(&conn, &pid)?
            .into_iter()
            .map(export_out_from)
            .collect())
    }

    /// Create an export record and enqueue the `run_export` job.
    pub fn create_export(&self, project_id: &str, request: ExportRequest) -> Result<ExportOut> {
        let pid = ProjectId::new(project_id)?;
        let format = request.format.clone().unwrap_or_else(|| "txt".to_string());
        if export::export_extension(&format).is_none() {
            return Err(Error::Unsupported(format!(
                "Unsupported export format: {format}"
            )));
        }
        let (book, src, tgt) = self.resolve_book(&pid)?;
        let export_id = new_id("x")?;
        let options_json = serde_json::to_value(&request)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "{}".to_string());
        let conn = self.conn()?;
        let record =
            store::create_export(&conn, &export_id, &pid, &book.id, &format, &options_json)?;
        let job_id = JobId::new(new_id("j")?)?;
        let run_id = RunId::new(new_id("r")?)?;
        let mut params = serde_json::Map::new();
        params.insert(
            "db_path".to_string(),
            serde_json::Value::String(self.db_path.to_string_lossy().into_owned()),
        );
        params.insert(
            "book_id".to_string(),
            serde_json::Value::String(book.id.as_str().to_string()),
        );
        params.insert("source_lang".to_string(), serde_json::Value::String(src));
        params.insert("target_lang".to_string(), serde_json::Value::String(tgt));
        params.insert(
            "export_id".to_string(),
            serde_json::Value::String(export_id.clone()),
        );
        params.insert("format".to_string(), serde_json::Value::String(format));
        params.insert(
            "options".to_string(),
            serde_json::from_str(&options_json).unwrap_or(serde_json::Value::Null),
        );
        params.insert(
            "out_dir".to_string(),
            serde_json::Value::String(
                self.dir
                    .join(EXPORTS_DIRNAME)
                    .to_string_lossy()
                    .into_owned(),
            ),
        );
        let params_json = serde_json::Value::Object(params).to_string();
        store::enqueue_job(&conn, &job_id, &pid, "export", &run_id, &params_json)?;
        store::set_export_run(&conn, &export_id, &run_id)?;
        store::set_export_status(&conn, &export_id, "queued", None, None, None)?;
        drop(conn);
        let params: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&params_json).unwrap_or_default();
        self.jobs
            .admit(&pid, "export", "run_export", &job_id, &run_id, params)?;
        let conn = self.conn()?;
        store::get_export(&conn, &export_id)?
            .map(export_out_from)
            .ok_or_else(|| Error::NotFound {
                kind: "export",
                id: export_id.clone(),
            })
            .or_else(|_| Ok(export_out_from(record)))
    }

    /// Finished export file bytes.
    pub fn export_bytes(&self, export_id: &str) -> Result<Vec<u8>> {
        let conn = self.conn()?;
        let record = store::get_export(&conn, export_id)?.ok_or_else(|| Error::NotFound {
            kind: "export",
            id: export_id.to_string(),
        })?;
        if record.status != "done" {
            return Err(Error::Unsupported(format!(
                "export {export_id} is not ready (status {})",
                record.status
            )));
        }
        let Some(path) = record.path.clone() else {
            return Err(Error::NotFound {
                kind: "export_file",
                id: export_id.to_string(),
            });
        };
        std::fs::read(&path).map_err(|e| Error::Io {
            path: PathBuf::from(path),
            message: e.to_string(),
        })
    }

    /// Suggested download filename for an export record.
    pub fn export_filename(&self, record: &ExportOut) -> String {
        let stem = export::sanitize_filename(
            &format!("{}-{}", record.project_id, record.id),
            "translated",
        );
        let ext = export::export_extension(&record.format).unwrap_or(".bin");
        format!("{stem}{ext}")
    }

    // -- Settings ------------------------------------------------------

    /// Global config: YAML text + effective JSON.
    pub fn get_global_config(&self) -> Result<GlobalConfigOut> {
        let path = self.dir.join(CONFIG_FILENAME);
        let yaml = std::fs::read_to_string(&path).unwrap_or_default();
        let cfg = self.config.lock().expect("service config").clone();
        Ok(GlobalConfigOut {
            yaml,
            effective: serde_json::to_value(&cfg).unwrap_or(serde_json::Value::Null),
            default_template: default_config_template(),
            revision: *self.revision.lock().expect("service revision"),
        })
    }

    /// Dry-run validation: parse YAML without persisting anything.
    pub fn validate_global_config(&self, yaml: &str) -> Result<GlobalConfigOut> {
        let cfg = config::load(yaml)?;
        Ok(GlobalConfigOut {
            yaml: yaml.to_string(),
            effective: serde_json::to_value(&cfg).unwrap_or(serde_json::Value::Null),
            default_template: default_config_template(),
            revision: *self.revision.lock().expect("service revision"),
        })
    }

    /// Built-in defaults (restores the Advanced/Footer template).
    /// `default_template` doubles as the YAML template; `revision` is the
    /// live in-memory revision (a save invalidates older baselines).
    pub fn get_global_defaults() -> GlobalConfigOut {
        let cfg = config::Config::default();
        let template = default_config_template();
        GlobalConfigOut {
            yaml: template.clone(),
            effective: serde_json::to_value(&cfg).unwrap_or(serde_json::Value::Null),
            default_template: template,
            revision: 1,
        }
    }

    /// Save global config YAML (last-write-wins), reload in-memory.
    pub fn save_global_config(&self, yaml: &str) -> Result<GlobalConfigOut> {
        let cfg = config::load(yaml)?;
        std::fs::write(self.dir.join(CONFIG_FILENAME), yaml).map_err(|e| Error::Io {
            path: self.dir.join(CONFIG_FILENAME),
            message: e.to_string(),
        })?;
        *self.config.lock().expect("service config") = cfg;
        *self.revision.lock().expect("service revision") += 1;
        self.get_global_config()
    }

    /// Revision-guarded save: 409-style [`Error::Conflict`] when the caller
    /// edited against a stale baseline.
    pub fn save_global_config_checked(
        &self,
        yaml: &str,
        expected_revision: i64,
    ) -> Result<GlobalConfigOut> {
        let current = *self.revision.lock().expect("service revision");
        if expected_revision != current {
            return Err(Error::Conflict(format!(
                "global config revision {expected_revision} is stale (current {current})"
            )));
        }
        self.save_global_config(yaml)
    }

    /// Credential presence per connection (session > env > missing).
    pub fn credential_statuses(&self) -> Vec<CredentialStatus> {
        let cfg = self.config.lock().expect("service config").clone();
        let session = self.credentials.lock().expect("service credentials");
        let mut out = Vec::new();
        for (name, provider) in &cfg.llm.providers {
            let kind = crate::llm::providers::ProviderKind::require(&provider.kind).ok();
            let env_var = provider
                .api_key_env
                .clone()
                .or_else(|| kind.and_then(|k| k.default_api_key_env().map(str::to_string)));
            let (configured, source) = if session.get(name).is_some_and(|secret| !secret.is_empty())
            {
                (true, "session")
            } else if env_var
                .as_deref()
                .is_some_and(|var| std::env::var(var).map(|v| !v.is_empty()).unwrap_or(false))
            {
                (true, "env")
            } else {
                (false, "missing")
            };
            out.push(CredentialStatus {
                connection: name.clone(),
                configured,
                source: source.to_string(),
            });
        }
        out
    }

    /// Store a session credential (memory only; never persisted).
    pub fn save_credential(&self, connection: &str, secret: &str) -> Result<()> {
        crate::redact::validate_credential(Some(secret))?;
        let cfg = self.config.lock().expect("service config").clone();
        if !cfg.llm.providers.contains_key(connection) {
            return Err(Error::NotFound {
                kind: "connection",
                id: connection.to_string(),
            });
        }
        self.credentials
            .lock()
            .expect("service credentials")
            .insert(connection.to_string(), secret.to_string());
        Ok(())
    }

    /// List models per connection snapshot with a timeout budget.
    pub async fn list_models(&self, connection: &str) -> Result<Vec<String>> {
        let (cfg, snapshot) = self.snapshot_config();
        let client = crate::llm::client::RoutedClient::new(
            cfg.llm,
            Some(snapshot),
            crate::llm::transport::silent_emit(),
        )?;
        client.list_models(connection).await
    }

    /// Ping every configured connection; one entry per connection.
    pub async fn ping_models(&self, timeout_secs: f64) -> Vec<ModelPing> {
        let (cfg, snapshot) = self.snapshot_config();
        let mut out = Vec::new();
        for connection in cfg.llm.providers.keys().cloned().collect::<Vec<_>>() {
            let budget = Duration::from_secs_f64(timeout_secs.clamp(0.5, 120.0));
            let client = crate::llm::client::RoutedClient::new(
                cfg.llm.clone(),
                Some(snapshot.clone()),
                crate::llm::transport::silent_emit(),
            );
            let result = match client {
                Err(error) => Err(error.to_string()),
                Ok(client) => {
                    match tokio::time::timeout(budget, client.list_models(&connection)).await {
                        Err(_) => Err("ping timed out".to_string()),
                        Ok(Err(error)) => Err(error.to_string()),
                        Ok(Ok(models)) => Ok(models),
                    }
                }
            };
            match result {
                Ok(models) => out.push(ModelPing {
                    connection,
                    ok: true,
                    models,
                    error: None,
                }),
                Err(error) => out.push(ModelPing {
                    connection,
                    ok: false,
                    models: Vec::new(),
                    error: Some(error),
                }),
            }
        }
        out
    }

    fn snapshot_config(&self) -> (config::Config, BTreeMap<String, Option<String>>) {
        let cfg = self.config.lock().expect("service config").clone();
        let session = self.credentials.lock().expect("service credentials");
        let mut snapshot = BTreeMap::new();
        for name in cfg.llm.providers.keys() {
            snapshot.insert(name.clone(), session.get(name).cloned());
        }
        // Env fallback: fill from process env when no session secret is set.
        for (name, provider) in &cfg.llm.providers {
            if snapshot.get(name).and_then(|v| v.clone()).is_some() {
                continue;
            }
            let kind = crate::llm::providers::ProviderKind::require(&provider.kind).ok();
            let env_var = provider
                .api_key_env
                .clone()
                .or_else(|| kind.and_then(|k| k.default_api_key_env().map(str::to_string)));
            if let Some(var) = env_var {
                if let Ok(secret) = std::env::var(&var) {
                    if !secret.is_empty() {
                        snapshot.insert(name.clone(), Some(secret));
                    }
                }
            }
        }
        (cfg, snapshot)
    }

    /// Static capabilities: languages, formats, provider kinds, operations.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            languages: i18n::LANGUAGES
                .iter()
                .map(|code| Language {
                    code: code.to_string(),
                    name: language_name(code),
                })
                .collect(),
            input_formats: vec![
                "epub".to_string(),
                "txt".to_string(),
                "md".to_string(),
                "html".to_string(),
            ],
            output_formats: export::EXPORT_FORMATS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            provider_kinds: provider_kinds(),
            operations: operation_caps(),
        }
    }
}

/// Operations from the `llm::operations` registry with resolved tier
/// defaults (inherits-chains followed). Sorted by id; never empty.
fn operation_caps() -> Vec<OperationCap> {
    crate::llm::operations::operation_ids()
        .into_iter()
        .map(|id| {
            let (tier, description) = crate::llm::operations::require_operation(id)
                .map(|spec| {
                    let tier = spec.tier.unwrap_or_else(|| resolve_inherited_tier(id));
                    (tier.to_string(), spec.description.to_string())
                })
                .unwrap_or_else(|_| ("strong".to_string(), String::new()));
            OperationCap {
                id: id.to_string(),
                tier,
                description,
            }
        })
        .collect()
}

/// Follow `inherits` parents to the concrete tier default.
fn resolve_inherited_tier(id: &str) -> &'static str {
    let mut current = id;
    for _ in 0..16 {
        let Ok(spec) = crate::llm::operations::require_operation(current) else {
            break;
        };
        if let Some(tier) = spec.tier {
            return tier;
        }
        match spec.inherits {
            Some(parent) => current = parent,
            None => break,
        }
    }
    "strong"
}

/// YAML template for a fresh global config (Advanced/Footer restore).
fn default_config_template() -> String {
    let cfg = config::Config::default();
    serde_json::to_value(&cfg)
        .ok()
        .and_then(|value| serde_yaml::to_string(&value).ok())
        .unwrap_or_default()
}

fn provider_kinds() -> Vec<String> {
    [
        "deepseek",
        "openai",
        "openrouter",
        "opencode-go",
        "openai-compatible",
        "orcarouter",
        "ollama",
        "vllm",
        "llamacpp",
        "gemini",
        "fake",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn language_name(code: &str) -> String {
    match code {
        "zh" => "Chinese".to_string(),
        "en" => "English".to_string(),
        "ja" => "Japanese".to_string(),
        "ko" => "Korean".to_string(),
        "fr" => "French".to_string(),
        "de" => "German".to_string(),
        "es" => "Spanish".to_string(),
        "it" => "Italian".to_string(),
        "pt" => "Portuguese".to_string(),
        "ru" => "Russian".to_string(),
        "vi" => "Vietnamese".to_string(),
        "zh-Hant" => "Traditional Chinese".to_string(),
        "en-US" => "English (US)".to_string(),
        "en-GB" => "English (UK)".to_string(),
        "pt-BR" => "Portuguese (Brazil)".to_string(),
        "pt-PT" => "Portuguese (Portugal)".to_string(),
        other => other.to_string(),
    }
}

fn new_id(prefix: &str) -> Result<String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let rand: u32 = (nanos ^ (nanos >> 33) ^ std::process::id() as u128) as u32;
    Ok(format!("{prefix}-{nanos:x}-{rand:x}"))
}

fn load_workspace_config(dir: &Path) -> Result<config::Config> {
    let path = dir.join(CONFIG_FILENAME);
    match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => config::load(&text),
        _ => Ok(config::Config::default()),
    }
}

fn require_project(conn: &rusqlite::Connection, project_id: &ProjectId) -> Result<()> {
    if store::get_project(conn, project_id)?.is_none() {
        return Err(Error::NotFound {
            kind: "project",
            id: project_id.as_str().to_string(),
        });
    }
    Ok(())
}

fn single_book(conn: &rusqlite::Connection, project_id: &ProjectId) -> Result<store::Book> {
    let books = store::list_books(conn, project_id)?;
    books.into_iter().next().ok_or_else(|| Error::NotFound {
        kind: "book",
        id: project_id.as_str().to_string(),
    })
}

/// Latest `paused`/`interrupted`/`error` job for idle-state reporting.
fn last_terminal_status(
    conn: &rusqlite::Connection,
    project_id: &ProjectId,
) -> Option<(String, String, String)> {
    let mut stmt = conn
        .prepare(
            "SELECT status, run_id, kind FROM jobs WHERE project_id = ? \
             AND status IN ('paused', 'interrupted', 'error') \
             ORDER BY updated_at DESC LIMIT 1",
        )
        .ok()?;
    let (status, run_id, kind): (String, String, String) = stmt
        .query_row([project_id.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .ok()?;
    Some((status, run_id, kind))
}

fn find_chapter(
    conn: &rusqlite::Connection,
    book_id: &BookId,
    ordinal: i64,
) -> Result<store::Chapter> {
    store::list_chapters(conn, book_id, 0, i64::MAX)?
        .into_iter()
        .find(|chapter| chapter.ordinal == ordinal)
        .ok_or_else(|| Error::NotFound {
            kind: "chapter",
            id: ordinal.to_string(),
        })
}

fn find_segment(
    conn: &rusqlite::Connection,
    chapter_id: &ChapterId,
    ordinal: i64,
) -> Result<store::Segment> {
    store::list_segments(conn, chapter_id, 0, i64::MAX)?
        .into_iter()
        .find(|segment| segment.ordinal == ordinal)
        .ok_or_else(|| Error::NotFound {
            kind: "segment",
            id: ordinal.to_string(),
        })
}

fn project_languages(
    conn: &rusqlite::Connection,
    project_id: &ProjectId,
    fallback: &config::Config,
) -> Result<(String, String)> {
    if let Some(text) = store::load_artifact(conn, project_id, "languages")? {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            let src = value
                .get("source_lang")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let tgt = value
                .get("target_lang")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !src.is_empty() && !tgt.is_empty() {
                return Ok((src, tgt));
            }
        }
    }
    Ok((fallback.source_lang.clone(), fallback.target_lang.clone()))
}

fn project_row(row: &store::Project) -> Project {
    Project {
        id: row.id.as_str().to_string(),
        name: row.name.clone(),
        title: None,
        fmt: None,
        source_lang: None,
        target_lang: None,
        status: String::new(),
        error: None,
        initialized: true,
        created_at: Some(row.created_at.to_string()),
    }
}

/// Full list row: languages, format, title, live status, ISO date.
fn project_row_full(
    conn: &rusqlite::Connection,
    row: &store::Project,
    config: &config::Config,
) -> Result<Project> {
    let (src, tgt) = project_languages(conn, &row.id, config)?;
    let books = store::list_books(conn, &row.id)?;
    let (title, fmt) = match books.first() {
        None => (None, None),
        Some(book) => {
            let fmt = store::list_source_files(conn, &book.id)?
                .into_iter()
                .next()
                .map(|file| file.fmt)
                .filter(|fmt| !fmt.is_empty());
            (Some(book.title.clone()), fmt)
        }
    };
    let active = store::list_active_jobs_with_params(conn, &row.id)?;
    let status = match active.first() {
        Some((job, _)) => job.status.clone(),
        None => last_terminal_status(conn, &row.id)
            .map(|(status, _, _)| status)
            .unwrap_or_else(|| "idle".to_string()),
    };
    Ok(Project {
        id: row.id.as_str().to_string(),
        name: row.name.clone(),
        title,
        fmt,
        source_lang: Some(src),
        target_lang: Some(tgt),
        status,
        error: None,
        initialized: !books.is_empty(),
        created_at: Some(ms_to_iso_date(row.created_at)),
    })
}

/// Epoch ms → `YYYY-MM-DD` (UTC) for list display.
fn ms_to_iso_date(ms: i64) -> String {
    const DAY_MS: i64 = 86_400_000;
    let days = ms.div_euclid(DAY_MS);
    // Howard Hinnant's civil-from-days (UTC, proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Convert a stored review run to its DTO form.
fn review_run_out_from(run: store::ReviewRun) -> ReviewRunOut {
    let issues = serde_json::from_str(&run.issues_json).unwrap_or_default();
    let summary = serde_json::from_str(&run.summary_json).unwrap_or(serde_json::Value::Null);
    ReviewRunOut {
        id: run.id,
        status: run.status,
        termination: run.termination,
        issues,
        summary,
    }
}

/// Issue counts per chapter ordinal from the latest finished review.
fn review_issues_by_chapter(review: Option<&store::ReviewRun>) -> BTreeMap<i64, u32> {
    let mut out = BTreeMap::new();
    let Some(run) = review else {
        return out;
    };
    let Ok(serde_json::Value::Array(issues)) = serde_json::from_str(&run.issues_json) else {
        return out;
    };
    for issue in issues {
        if let Some(chapter) = issue.get("chapter").and_then(|v| v.as_i64()) {
            *out.entry(chapter).or_insert(0) += 1;
        }
    }
    out
}

fn project_detail(conn: &rusqlite::Connection, row: &store::Project) -> Result<ProjectDetail> {
    let books = store::list_books(conn, &row.id)?;
    let (title, chapter_count, total_words, done) = match books.first() {
        None => (None, 0u32, 0u64, 0u32),
        Some(book) => {
            // Header rows + cached counts — no text scan.
            let chapters = store::list_chapters(conn, &book.id, 0, i64::MAX)?;
            let (words, _) = store::cached_book_word_totals(conn, &book.id)?;
            let done_chapters = chapters.iter().filter(|c| c.status == "done").count() as u32;
            (
                Some(book.title.clone()),
                chapters.len() as u32,
                words,
                done_chapters,
            )
        }
    };
    let (source_lang, target_lang) = project_languages_opt(conn, &row.id);
    let mut project = project_row(row);
    project.title = title.clone();
    project.source_lang = source_lang;
    project.target_lang = target_lang;
    Ok(ProjectDetail {
        project,
        book_title: title,
        chapter_count,
        total_word_count: total_words,
        done_chapters: done,
    })
}

fn project_languages_opt(
    conn: &rusqlite::Connection,
    project_id: &ProjectId,
) -> (Option<String>, Option<String>) {
    store::load_artifact(conn, project_id, "languages")
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .map(|value| {
            (
                value
                    .get("source_lang")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                value
                    .get("target_lang")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            )
        })
        .unwrap_or((None, None))
}

fn glossary_term_from(term: glossary::GlossaryTerm) -> GlossaryTerm {
    GlossaryTerm {
        source: term.source,
        target: term.target,
        reading: term.reading,
        term_type: term.term_type,
        gender: term.gender,
        aliases: term.aliases,
        first_chapter: term.first_chapter,
        note: term.note,
        status: term.status,
        mentions: term.mentions,
    }
}

fn glossary_term_into(term: TermIn) -> glossary::GlossaryTerm {
    glossary::GlossaryTerm {
        source: term.source,
        target: term.target,
        reading: term.reading,
        term_type: term.term_type,
        gender: term.gender,
        aliases: term.aliases,
        first_chapter: None,
        note: term.note,
        status: "ok".to_string(),
        mentions: 0,
    }
}

fn conflict_ids(
    conn: &rusqlite::Connection,
    project_id: &ProjectId,
    src: &str,
    tgt: &str,
) -> Result<Vec<i64>> {
    let mut stmt = conn
        .prepare(
            "SELECT id FROM glossary_conflicts WHERE project_id = ? AND source_lang = ? \
             AND target_lang = ? AND resolved = 0 ORDER BY created_at",
        )
        .map_err(store::map_error)?;
    let rows = stmt
        .query_map(rusqlite::params![project_id.as_str(), src, tgt], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(store::map_error)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(store::map_error)?);
    }
    Ok(out)
}

fn export_out_from(record: store::ExportRecord) -> ExportOut {
    ExportOut {
        options: serde_json::from_str(&record.options_json).unwrap_or(serde_json::Value::Null),
        error: record.error,
        id: record.id,
        project_id: record.project_id.as_str().to_string(),
        format: record.format,
        status: record.status,
        path: record.path,
        size: record.bytes,
        created_at: Some(record.created_at.to_string()),
    }
}

mod tasks {
    use super::*;

    fn params_db_path(work: &jobs::WorkItem) -> Result<PathBuf> {
        work.params
            .get("db_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .ok_or_else(|| Error::Config("task is missing its database path".to_string()))
    }

    fn params_langs(work: &jobs::WorkItem) -> (String, String) {
        (
            work.params
                .get("source_lang")
                .and_then(|v| v.as_str())
                .unwrap_or("auto")
                .to_string(),
            work.params
                .get("target_lang")
                .and_then(|v| v.as_str())
                .unwrap_or("zh")
                .to_string(),
        )
    }

    fn params_config(work: &jobs::WorkItem) -> config::Config {
        work.params
            .get("config_yaml")
            .and_then(|v| v.as_str())
            .filter(|text| !text.trim().is_empty())
            .and_then(|text| config::load(text).ok())
            .unwrap_or_default()
    }

    fn fake_client() -> crate::llm::fake::FakeClient {
        // v1 default: deterministic offline client. RoutedClient wiring lands
        // when provider credentials + routing move in-process.
        crate::llm::fake::FakeClient::new(Some(demo_handler()), None).expect("fake client")
    }

    /// Count `[n] ...` source lines in the user prompt.
    fn count_numbered(text: &str) -> usize {
        text.lines()
            .filter(|line| {
                line.starts_with('[') && line.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
            })
            .count()
    }

    /// Deterministic demo responses (mirror of `tests/pipeline_translate.rs`).
    /// Clearly synthetic output — real providers replace this in v1+.
    fn demo_handler() -> crate::llm::fake::FakeHandler {
        Box::new(
            |messages: &[crate::llm::compat::Message], _tier: &str, json_mode: bool| {
                let system = messages.first().map(|m| m.content.as_str()).unwrap_or("");
                let user = messages.get(1).map(|m| m.content.as_str()).unwrap_or("");
                if system.contains("pre-translation analyst") {
                    return serde_json::json!({
                        "genre": "demo",
                        "tone": "neutral",
                        "style_guide": "plain",
                        "characters": [],
                        "terms": [],
                    })
                    .to_string();
                }
                if system.contains("chapter title translator") {
                    let n = count_numbered(user);
                    return serde_json::json!({
                        "titles": (0..n).map(|i| format!("[demo] title {i}")).collect::<Vec<_>>(),
                    })
                    .to_string();
                }
                if system.contains("literary translator") {
                    if user.contains("Polish the translations from your previous JSON response") {
                        let mut n = None;
                        for message in messages[..messages.len().saturating_sub(1)].iter().rev() {
                            if message.role == "assistant" {
                                let payload: serde_json::Value =
                                    serde_json::from_str(&message.content).unwrap_or_default();
                                if let Some(items) =
                                    payload.get("translations").and_then(|v| v.as_array())
                                {
                                    n = Some(items.len());
                                }
                                break;
                            }
                        }
                        let n = n.unwrap_or_else(|| count_numbered(user));
                        return serde_json::json!({
                        "polished": (0..n).map(|i| format!("[demo] polished {i}")).collect::<Vec<_>>(),
                    })
                    .to_string();
                    }
                    let n = count_numbered(user);
                    return serde_json::json!({
                    "translations": (0..n).map(|i| format!("[demo] translation {i}")).collect::<Vec<_>>(),
                })
                .to_string();
                }
                if system.contains("prose editor") {
                    let n = count_numbered(user);
                    return serde_json::json!({
                    "polished": (0..n).map(|i| format!("[demo] polished {i}")).collect::<Vec<_>>(),
                })
                .to_string();
                }
                if system.contains("chapter digest writer") {
                    return "[demo] chapter digest.".to_string();
                }
                if system.contains("whole-book synopsis writer") {
                    return "[demo] book synopsis.".to_string();
                }
                if json_mode {
                    "{}".to_string()
                } else {
                    String::new()
                }
            },
        )
    }

    fn options_from_json(value: Option<&serde_json::Value>) -> AssembleOptions {
        let get_bool = |key: &str| {
            value
                .and_then(|v| v.get(key))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        };
        let get_str = |key: &str, fallback: &str| {
            value
                .and_then(|v| v.get(key))
                .and_then(|v| v.as_str())
                .unwrap_or(fallback)
                .to_string()
        };
        AssembleOptions {
            bilingual: get_bool("bilingual"),
            order: get_str("order", "target_first"),
            preserve_source_style: get_bool("preserve_source_style"),
            preserve_source_ruby: value
                .and_then(|v| v.get("preserve_source_ruby"))
                .and_then(|v| v.as_bool()),
        }
    }

    fn run_guarded(
        work: &jobs::WorkItem,
        cancel: &jobs::Cancel,
        body: impl FnOnce(
            &rusqlite::Connection,
            &config::Config,
            &mut crate::llm::fake::FakeClient,
        ) -> Result<jobs::TaskOutcome>,
    ) -> jobs::TaskOutcome {
        let run = || {
            let db_path = params_db_path(work)?;
            let conn = store::open(&db_path)?;
            let cfg = params_config(work);
            let mut client = fake_client();
            body(&conn, &cfg, &mut client)
        };
        match run() {
            Ok(jobs::TaskOutcome::Done) if cancel.is_cancelled() => {
                jobs::TaskOutcome::Interrupted("The local task was cancelled.".to_string())
            }
            Ok(outcome) => outcome,
            Err(error) => jobs::TaskOutcome::Failed(error.to_string()),
        }
    }

    pub fn translation_task() -> jobs::TaskFn {
        Arc::new(move |work: jobs::WorkItem, cancel: jobs::Cancel| {
            Box::pin(async move {
                run_guarded(&work, &cancel, |conn, cfg, client| {
                    let (src, tgt) = params_langs(&work);
                    let book_id = work
                        .params
                        .get("book_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            Error::Config("translation task is missing book_id".to_string())
                        })?;
                    let book_id = BookId::new(book_id)?;
                    let mut cfg = cfg.clone();
                    cfg.source_lang = src;
                    cfg.target_lang = tgt;
                    let mut pipeline = crate::pipeline::Pipeline::new(cfg, client);
                    let synopsis = store::load_synopsis(conn, &book_id)?
                        .map(|(s, _)| s)
                        .unwrap_or_default();
                    let mut events = Vec::new();
                    pipeline.translate(
                        conn,
                        &work.project_id,
                        &book_id,
                        &synopsis,
                        None,
                        &mut crate::pipeline::Progress::none(),
                        &mut events,
                    )?;
                    for event in &events {
                        let kind = event
                            .get("event")
                            .and_then(|v| v.as_str())
                            .unwrap_or("translate_event");
                        let _ =
                            store::append_job_event(conn, &work.job_id, kind, &event.to_string());
                    }
                    if cancel.is_cancelled() {
                        return Ok(jobs::TaskOutcome::Interrupted(
                            "The translation was cancelled.".to_string(),
                        ));
                    }
                    Ok(jobs::TaskOutcome::Done)
                })
            }) as jobs::futures::TaskFuture
        })
    }

    pub fn chapter_translation_task() -> jobs::TaskFn {
        Arc::new(move |work: jobs::WorkItem, cancel: jobs::Cancel| {
            Box::pin(async move {
                run_guarded(&work, &cancel, |conn, cfg, client| {
                    let (src, tgt) = params_langs(&work);
                    let book_id = work
                        .params
                        .get("book_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            Error::Config("chapter translation task is missing book_id".to_string())
                        })?;
                    let book_id = BookId::new(book_id)?;
                    let chapter_index = work
                        .params
                        .get("chapter_index")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let mut cfg = cfg.clone();
                    cfg.source_lang = src;
                    cfg.target_lang = tgt;
                    let mut pipeline = crate::pipeline::Pipeline::new(cfg, client);
                    let synopsis = store::load_synopsis(conn, &book_id)?
                        .map(|(s, _)| s)
                        .unwrap_or_default();
                    let mut events = Vec::new();
                    pipeline.translate(
                        conn,
                        &work.project_id,
                        &book_id,
                        &synopsis,
                        Some(chapter_index),
                        &mut crate::pipeline::Progress::none(),
                        &mut events,
                    )?;
                    for event in &events {
                        let kind = event
                            .get("event")
                            .and_then(|v| v.as_str())
                            .unwrap_or("translate_event");
                        let _ =
                            store::append_job_event(conn, &work.job_id, kind, &event.to_string());
                    }
                    if cancel.is_cancelled() {
                        return Ok(jobs::TaskOutcome::Interrupted(
                            "The chapter translation was cancelled.".to_string(),
                        ));
                    }
                    Ok(jobs::TaskOutcome::Done)
                })
            }) as jobs::futures::TaskFuture
        })
    }

    pub fn export_task() -> jobs::TaskFn {
        Arc::new(move |work: jobs::WorkItem, cancel: jobs::Cancel| {
            Box::pin(async move {
                run_guarded(&work, &cancel, |conn, _cfg, _client| {
                    let export_id = work
                        .params
                        .get("export_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            Error::Config("export task is missing export_id".to_string())
                        })?;
                    let book_id = work
                        .params
                        .get("book_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            Error::Config("export task is missing book_id".to_string())
                        })?;
                    let book_id = BookId::new(book_id)?;
                    let format = work
                        .params
                        .get("format")
                        .and_then(|v| v.as_str())
                        .unwrap_or("txt");
                    let (src, tgt) = params_langs(&work);
                    let options = options_from_json(work.params.get("options"));
                    let out_dir = work
                        .params
                        .get("out_dir")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from("."));
                    std::fs::create_dir_all(&out_dir).map_err(|e| Error::Io {
                        path: out_dir.clone(),
                        message: e.to_string(),
                    })?;
                    let ext = export::export_extension(format).unwrap_or(".bin");
                    let out_path = out_dir.join(format!("{export_id}{ext}"));
                    let book = store::list_books(conn, &work.project_id)?
                        .into_iter()
                        .find(|b| b.id == book_id)
                        .ok_or_else(|| Error::NotFound {
                            kind: "book",
                            id: book_id.as_str().to_string(),
                        })?;
                    let source_stub = PathBuf::from("source");
                    let mut events = Vec::new();
                    match export::export_book(
                        conn,
                        &book_id,
                        &source_stub,
                        Some(&out_path),
                        format,
                        Some(&book.title),
                        &src,
                        &tgt,
                        "text",
                        &book.title,
                        &options,
                        None,
                        &mut events,
                    ) {
                        Ok(path) => {
                            let bytes = std::fs::metadata(&path).map(|m| m.len() as i64).ok();
                            let path_str = path.to_string_lossy().into_owned();
                            let _ = store::set_export_status(
                                conn,
                                export_id,
                                "done",
                                Some(&path_str),
                                bytes,
                                None,
                            );
                            let _ = store::append_job_event(
                                conn,
                                &work.job_id,
                                "export_done",
                                &serde_json::json!({"path": path_str}).to_string(),
                            );
                        }
                        Err(error) => {
                            let _ = store::set_export_status(
                                conn,
                                export_id,
                                "error",
                                None,
                                None,
                                Some(&error.to_string()),
                            );
                            return Ok(jobs::TaskOutcome::Failed(error.to_string()));
                        }
                    }
                    if cancel.is_cancelled() {
                        return Ok(jobs::TaskOutcome::Interrupted(
                            "The export was cancelled.".to_string(),
                        ));
                    }
                    Ok(jobs::TaskOutcome::Done)
                })
            }) as jobs::futures::TaskFuture
        })
    }

    pub fn refresh_analysis_task() -> jobs::TaskFn {
        Arc::new(move |work: jobs::WorkItem, cancel: jobs::Cancel| {
            Box::pin(async move {
                run_guarded(&work, &cancel, |conn, cfg, client| {
                    let (src, tgt) = params_langs(&work);
                    let book_id = work
                        .params
                        .get("book_id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            Error::Config("refresh task is missing book_id".to_string())
                        })?;
                    let book_id = BookId::new(book_id)?;
                    let mut cfg = cfg.clone();
                    cfg.source_lang = src.clone();
                    cfg.target_lang = tgt.clone();
                    let plan = crate::i18n::policy::resolve_policy(
                        &src,
                        &tgt,
                        "analysis",
                        &[],
                        &cfg.honorific_strategy,
                    )
                    .or_else(|_| {
                        crate::pipeline::Pipeline::new(cfg.clone(), client).plan("analysis")
                    })?;
                    let analyzer = crate::pipeline_agents::Analyzer::new(&src, &tgt, plan, &cfg);
                    let chapters = store::list_chapters(conn, &book_id, 0, i64::MAX)?;
                    let mut sample_parts = Vec::new();
                    for chapter in chapters.iter().take(3) {
                        for segment in store::list_segments(conn, &chapter.id, 0, 64)?
                            .into_iter()
                            .take(8)
                        {
                            if !segment.source.trim().is_empty() {
                                sample_parts.push(segment.source.clone());
                            }
                        }
                    }
                    let sample = sample_parts.join("\n");
                    if !sample.trim().is_empty() {
                        let analysis = analyzer.analyze(client, &sample)?;
                        store::save_analysis(
                            conn,
                            &book_id,
                            &serde_json::Value::Object(analysis.clone()).to_string(),
                        )?;
                        let _ = style::seed_from_analysis(
                            conn,
                            &work.project_id,
                            &book_id,
                            &src,
                            &tgt,
                            &analysis,
                            "refresh",
                        );
                    }
                    let _ = store::append_job_event(
                        conn,
                        &work.job_id,
                        "analysis_refreshed",
                        &serde_json::json!({}).to_string(),
                    );
                    if cancel.is_cancelled() {
                        return Ok(jobs::TaskOutcome::Interrupted(
                            "The analysis refresh was cancelled.".to_string(),
                        ));
                    }
                    Ok(jobs::TaskOutcome::Done)
                })
            }) as jobs::futures::TaskFuture
        })
    }

    /// Background glossary review (`Curator::task` with the demo client).
    /// The demo handler has no curator branch, so passes verify terms
    /// without filing new conflicts — enough to exercise the durable
    /// state machine (`curator_state`, stop flag, history events).
    pub fn curator_task() -> jobs::TaskFn {
        crate::curator::Curator::task(crate::curator::Curator::new("", ""), || {
            Box::new(fake_client_for_curator()) as Box<dyn crate::pipeline_agents::ModelClient>
        })
    }

    fn fake_client_for_curator() -> crate::llm::fake::FakeClient {
        crate::llm::fake::FakeClient::new(Some(demo_handler()), None).expect("fake client")
    }
}
pub mod import {
    use super::*;

    /// Persist raw file bytes and dispatch by extension (v1 import set).
    pub fn import_bytes(
        conn: &rusqlite::Connection,
        project_id: &ProjectId,
        src: &str,
        tgt: &str,
        file_bytes: &[u8],
        filename: &str,
    ) -> Result<(BookId, String)> {
        let ext = Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let fmt = match ext.as_str() {
            "epub" => "epub",
            "txt" => "text",
            "md" | "markdown" => "text",
            "html" | "htm" | "xhtml" => "html",
            "srt" => "srt",
            "docx" | "fb2" | "pdf" => {
                return Err(Error::Unsupported(format!(
                    "import of .{ext} is not supported in v1 (use epub/txt/md/html)"
                )));
            }
            other => {
                return Err(Error::Unsupported(format!(
                    "unsupported import format .{other} (use epub/txt/md/html)"
                )));
            }
        };
        let staged = stage_bytes(project_id, file_bytes, filename)?;
        // SRT parses (engine stays fmt=srt capable); raw text persists as source.
        if ext == "srt" {
            let text = String::from_utf8_lossy(file_bytes);
            let _ = crate::document::srt::parse_srt_text(&text)?;
        }
        let books = store::list_books(conn, project_id)?;
        let ordinal = books.iter().map(|b| b.ordinal).max().unwrap_or(-1) + 1;
        let stem = format!("book-{ordinal:04}");
        let mut candidate = stem.clone();
        let mut attempt = 0u32;
        while books.iter().any(|b| b.id.as_str() == candidate) {
            attempt += 1;
            candidate = format!("{stem}-{attempt}");
        }
        let book_id = BookId::new(candidate)?;
        let title = Path::new(filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(filename)
            .to_string();
        store::create_book(conn, &book_id, project_id, ordinal, &title)?;
        let sha = sha256_hex(file_bytes);
        store::insert_source_file(
            conn,
            &format!("{}-src", book_id.as_str()),
            &book_id,
            filename,
            &sha,
            file_bytes.len() as i64,
            &ext,
        )?;
        let document = match ext.as_str() {
            "epub" => crate::document::reader::read_epub(&staged, src, tgt)?,
            "html" | "htm" | "xhtml" => crate::document::html::read_html(&staged, src, tgt, None)?,
            _ => crate::document::text::read_text(&staged, src, tgt)?,
        };
        for (index, chapter) in document.chapters.iter().enumerate() {
            let chapter_id = ChapterId::new(format!("{}-ch{index:04}", book_id.as_str()))?;
            let chapter_title = if chapter.title.trim().is_empty() {
                format!("Chapter {}", index + 1)
            } else {
                chapter.title.clone()
            };
            store::create_chapter(conn, &chapter_id, &book_id, index as i64, &chapter_title)?;
            let sources: Vec<(SegmentId, String)> = chapter
                .segments
                .iter()
                .enumerate()
                .filter(|(_, s)| !s.source.trim().is_empty())
                .map(|(i, s)| {
                    (
                        SegmentId::new(format!("{}-s{i:05}", chapter_id.as_str()))
                            .expect("segment id"),
                        s.source.clone(),
                    )
                })
                .collect();
            let pairs: Vec<(SegmentId, String)> = sources
                .iter()
                .map(|(id, text)| (id.clone(), text.clone()))
                .collect();
            store::insert_segments(conn, &chapter_id, &pairs)?;
            for (ord, doc_segment) in chapter
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
                        ord as i64,
                        &doc_segment.kind,
                        doc_segment.anchor.as_deref().unwrap_or(""),
                        doc_segment.resource_href.as_deref().unwrap_or(""),
                        doc_segment.cont,
                        &serde_json::Value::Object(doc_segment.meta.clone()).to_string(),
                    )?;
                }
            }
        }
        let _ = fmt;
        Ok((book_id, fmt.to_string()))
    }

    fn stage_bytes(project_id: &ProjectId, file_bytes: &[u8], filename: &str) -> Result<PathBuf> {
        let mut staged = std::env::temp_dir().join(format!(
            "wenyi-import-{}-{}",
            project_id.as_str(),
            std::process::id()
        ));
        std::fs::create_dir_all(&staged).map_err(|e| Error::Io {
            path: staged.clone(),
            message: e.to_string(),
        })?;
        staged.push(sanitize_filename(filename));
        std::fs::write(&staged, file_bytes).map_err(|e| Error::Io {
            path: staged.clone(),
            message: e.to_string(),
        })?;
        Ok(staged)
    }

    fn sanitize_filename(name: &str) -> String {
        let cleaned: String = name
            .chars()
            .map(|c| match c {
                '/' | '\\' => '_',
                _ if c.is_control() => '_',
                _ => c,
            })
            .collect();
        let trimmed = cleaned.trim().trim_matches('.');
        if trimmed.is_empty() {
            "input".to_string()
        } else {
            trimmed.to_string()
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex(&hasher.finalize())
    }

    fn hex(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push(DIGITS[(b >> 4) as usize] as char);
            out.push(DIGITS[(b & 0xf) as usize] as char);
        }
        out
    }
}
