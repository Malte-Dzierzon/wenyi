//! In-process engine facade for the GUI.
//!
//! Thin GPUI-friendly handle over `wenyi_core::service::Service`.
//! Sync engine calls run on background threads; the few async engine calls
//! (`start`, `shutdown`, `list_models`, `ping_models`) plus the event
//! `subscribe()` pump go through [`block_on`]. The handle caches
//! project-scoped reads (5s TTL) and capabilities (TTL-less, static per
//! process); mutations invalidate. DTOs and `ServiceError` are re-exported
//! from the engine — this module adds the shared Tokio runtime, the
//! cloneable handle, the default workspace dir, the busy-status helper,
//! and the in-process response cache.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use wenyi_core::service::dto::{
    AnalysisOut, Capabilities, ChapterSegments, ChapterSummary, CredentialStatus, CuratorStatus,
    EngineEvent, EventOut, ExportOut, ExportRequest, GlobalConfigOut, GlossaryTerm, ModelPing,
    Project, ProjectDetail, ProjectStats, ReviewRunOut, SegmentRevision, ServiceError,
    TermConflict, TermIn, WorkflowOut,
};
pub use wenyi_core::service::Service;

/// TTL for project-scoped read caches (`get_project`, `list_chapters`).
/// Mirrors the old sidecar client's `PROJECT_CACHE_TTL` (React `staleTime`
/// 5000): remounts within the window reuse data instead of refetching.
/// Entries are only read/written off the UI thread (inside
/// `background_spawn`); the mutex is never held across `.await`.
const PROJECT_CACHE_TTL: Duration = Duration::from_secs(5);

/// One cached value with its fetch time.
#[derive(Debug, Clone)]
struct Cached<T> {
    fetched_at: Instant,
    value: T,
}

impl<T> Cached<T> {
    fn fresh(value: T) -> Self {
        Self {
            fetched_at: Instant::now(),
            value,
        }
    }

    fn get(&self, ttl: Duration) -> Option<&T> {
        (self.fetched_at.elapsed() < ttl).then_some(&self.value)
    }
}

/// Shared response cache behind [`ServiceHandle`]. All handle clones (one
/// per view) point at the same store, so navigating between screens reuses
/// data instead of refetching.
#[derive(Debug, Default)]
struct CacheStore {
    projects: HashMap<String, Cached<ProjectDetail>>,
    chapters: HashMap<String, Cached<Vec<ChapterSummary>>>,
    /// `list_projects` result; same 5s TTL (dashboard re-entry).
    project_list: Option<Cached<Vec<Project>>>,
    /// `get_stats` ledger pair; same 5s TTL (overview status ping +
    /// snapshot legs). JSON values clone cheaply at ledger sizes.
    stats: HashMap<String, Cached<ProjectStats>>,
    /// `get_workflow` per project; same 5s TTL (overview snapshot leg).
    workflows: HashMap<String, Cached<WorkflowOut>>,
    /// `list_review_runs` per project; same 5s TTL (overview snapshot leg).
    review_runs: HashMap<String, Cached<Vec<ReviewRunOut>>>,
    /// `list_terms` per project; same 5s TTL (glossary re-entry + worker).
    terms: HashMap<String, Cached<Vec<GlossaryTerm>>>,
    /// `list_conflicts` per project; same 5s TTL (glossary re-entry).
    conflicts: HashMap<String, Cached<Vec<TermConflict>>>,
    /// `get_analysis` per project; same 5s TTL (style re-entry).
    analysis: HashMap<String, Cached<AnalysisOut>>,
    /// `capabilities` is static per process; no TTL.
    capabilities: Option<Capabilities>,
}

/// Shared Tokio runtime for the async engine calls. GPUI's executors are not
/// Tokio runtimes, so `start`/`shutdown`/`list_models`/`ping_models` and the
/// broadcast pump go through [`block_on`]. Must be called off the UI thread.
static RUNTIME: std::sync::LazyLock<tokio::runtime::Runtime> = std::sync::LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
});

/// Run an engine future to completion on the shared runtime.
/// Must be called off the UI thread.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    RUNTIME.block_on(fut)
}

/// Cloneable handle to the in-process [`Service`].
/// Constructed once at startup via [`ServiceHandle::open`]; cloned into
/// every view (the inner `Service` is `Arc`-shared).
#[derive(Clone)]
pub struct ServiceHandle {
    inner: Arc<Service>,
    cache: Arc<Mutex<CacheStore>>,
}

impl ServiceHandle {
    /// Open (or create) the workspace at `dir`: migrate `workspace.sqlite3`,
    /// load `config.yaml`, register jobs tasks. Callers must then run
    /// `block_on(service.start())` once before admitting jobs.
    pub fn open(dir: &std::path::Path) -> Result<Self, ServiceError> {
        Ok(Self {
            inner: Arc::new(Service::open(dir)?),
            cache: Arc::new(Mutex::new(CacheStore::default())),
        })
    }

    /// Resolve the default workspace dir: `$WENYI_NATIVE_DATA_DIR` when set,
    /// else the platform data dir (`~/.local/share/wenyi` on Linux).
    pub fn default_dir() -> std::path::PathBuf {
        if let Ok(dir) = std::env::var("WENYI_NATIVE_DATA_DIR") {
            if !dir.trim().is_empty() {
                return std::path::PathBuf::from(dir);
            }
        }
        dirs::data_dir()
            .map(|d| d.join("wenyi"))
            .unwrap_or_else(|| std::path::PathBuf::from(".wenyi-workspace"))
    }

    /// Drop all cached entries for `pid`. Called after any successful
    /// mutation; views can also call it explicitly.
    pub fn invalidate_project(&self, pid: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.projects.remove(pid);
            cache.chapters.remove(pid);
            cache.stats.remove(pid);
            cache.workflows.remove(pid);
            cache.review_runs.remove(pid);
            cache.terms.remove(pid);
            cache.conflicts.remove(pid);
            cache.analysis.remove(pid);
            // Project mutations (create/delete/status) also change the list.
            cache.project_list = None;
        }
    }

    /// `list_projects` — cached 5s across views (dashboard re-entry skips
    /// the refetch).
    pub fn list_projects(&self) -> Result<Vec<Project>, ServiceError> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(hit) = cache
                .project_list
                .as_ref()
                .and_then(|c| c.get(PROJECT_CACHE_TTL))
            {
                return Ok(hit.clone());
            }
        }
        let list = self.inner.list_projects()?;
        if let Ok(mut cache) = self.cache.lock() {
            cache.project_list = Some(Cached::fresh(list.clone()));
        }
        Ok(list)
    }

    /// `get_project` — cached 5s across all views.
    pub fn get_project(&self, pid: &str) -> Result<ProjectDetail, ServiceError> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(hit) = cache
                .projects
                .get(pid)
                .and_then(|c| c.get(PROJECT_CACHE_TTL))
            {
                return Ok(hit.clone());
            }
        }
        let detail = self.inner.get_project(pid)?;
        if let Ok(mut cache) = self.cache.lock() {
            cache
                .projects
                .insert(pid.to_string(), Cached::fresh(detail.clone()));
        }
        Ok(detail)
    }

    /// `list_chapters` — same 5s cross-view cache as [`Self::get_project`].
    pub fn list_chapters(&self, pid: &str) -> Result<Vec<ChapterSummary>, ServiceError> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(hit) = cache
                .chapters
                .get(pid)
                .and_then(|c| c.get(PROJECT_CACHE_TTL))
            {
                return Ok(hit.clone());
            }
        }
        let list = self.inner.list_chapters(pid)?;
        if let Ok(mut cache) = self.cache.lock() {
            cache
                .chapters
                .insert(pid.to_string(), Cached::fresh(list.clone()));
        }
        Ok(list)
    }

    /// `get_stats` — same 5s cross-view cache (overview snapshot + ping).
    pub fn get_stats(&self, pid: &str) -> Result<ProjectStats, ServiceError> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(hit) = cache.stats.get(pid).and_then(|c| c.get(PROJECT_CACHE_TTL)) {
                return Ok(hit.clone());
            }
        }
        let stats = self.inner.get_stats(pid)?;
        if let Ok(mut cache) = self.cache.lock() {
            cache
                .stats
                .insert(pid.to_string(), Cached::fresh(stats.clone()));
        }
        Ok(stats)
    }

    /// `get_workflow` — same 5s cross-view cache (overview snapshot leg).
    pub fn get_workflow(&self, pid: &str) -> Result<WorkflowOut, ServiceError> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(hit) = cache
                .workflows
                .get(pid)
                .and_then(|c| c.get(PROJECT_CACHE_TTL))
            {
                return Ok(hit.clone());
            }
        }
        let workflow = self.inner.get_workflow(pid)?;
        if let Ok(mut cache) = self.cache.lock() {
            cache
                .workflows
                .insert(pid.to_string(), Cached::fresh(workflow.clone()));
        }
        Ok(workflow)
    }

    /// `list_review_runs` — same 5s cross-view cache (overview snapshot leg).
    pub fn list_review_runs(&self, pid: &str) -> Result<Vec<ReviewRunOut>, ServiceError> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(hit) = cache
                .review_runs
                .get(pid)
                .and_then(|c| c.get(PROJECT_CACHE_TTL))
            {
                return Ok(hit.clone());
            }
        }
        let runs = self.inner.list_review_runs(pid)?;
        if let Ok(mut cache) = self.cache.lock() {
            cache
                .review_runs
                .insert(pid.to_string(), Cached::fresh(runs.clone()));
        }
        Ok(runs)
    }

    /// `list_terms` — same 5s cross-view cache (glossary re-entry + worker).
    pub fn list_terms(&self, pid: &str) -> Result<Vec<GlossaryTerm>, ServiceError> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(hit) = cache.terms.get(pid).and_then(|c| c.get(PROJECT_CACHE_TTL)) {
                return Ok(hit.clone());
            }
        }
        let terms = self.inner.list_terms(pid)?;
        if let Ok(mut cache) = self.cache.lock() {
            cache
                .terms
                .insert(pid.to_string(), Cached::fresh(terms.clone()));
        }
        Ok(terms)
    }

    /// `list_conflicts` — same 5s cross-view cache (glossary re-entry).
    pub fn list_conflicts(&self, pid: &str) -> Result<Vec<TermConflict>, ServiceError> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(hit) = cache
                .conflicts
                .get(pid)
                .and_then(|c| c.get(PROJECT_CACHE_TTL))
            {
                return Ok(hit.clone());
            }
        }
        let conflicts = self.inner.list_conflicts(pid)?;
        if let Ok(mut cache) = self.cache.lock() {
            cache
                .conflicts
                .insert(pid.to_string(), Cached::fresh(conflicts.clone()));
        }
        Ok(conflicts)
    }

    /// `get_analysis` — same 5s cross-view cache (style re-entry).
    pub fn get_analysis(&self, pid: &str) -> Result<AnalysisOut, ServiceError> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(hit) = cache
                .analysis
                .get(pid)
                .and_then(|c| c.get(PROJECT_CACHE_TTL))
            {
                return Ok(hit.clone());
            }
        }
        let analysis = self.inner.get_analysis(pid)?;
        if let Ok(mut cache) = self.cache.lock() {
            cache
                .analysis
                .insert(pid.to_string(), Cached::fresh(analysis.clone()));
        }
        Ok(analysis)
    }
    /// `capabilities` — static per process; fetched once per session.
    pub fn capabilities(&self) -> Capabilities {
        if let Ok(cache) = self.cache.lock() {
            if let Some(caps) = cache.capabilities.clone() {
                return caps;
            }
        }
        let caps = self.inner.capabilities();
        if let Ok(mut cache) = self.cache.lock() {
            cache.capabilities = Some(caps.clone());
        }
        caps
    }

    /// `create_project` — invalidates the project list after success.
    pub fn create_project(
        &self,
        name: &str,
        source_lang: &str,
        target_lang: &str,
        file_bytes: &[u8],
        filename: &str,
    ) -> Result<ProjectDetail, ServiceError> {
        let detail =
            self.inner
                .create_project(name, source_lang, target_lang, file_bytes, filename)?;
        self.invalidate_project(&detail.project.id);
        Ok(detail)
    }

    /// `delete_project` — invalidates the project + list after success.
    pub fn delete_project(&self, pid: &str) -> Result<(), ServiceError> {
        let out = self.inner.delete_project(pid).map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `edit_segment` — invalidates the project's cached reads.
    pub fn edit_segment(
        &self,
        pid: &str,
        chapter: u32,
        segment: u32,
        target: &str,
        expected: Option<&str>,
    ) -> Result<(), ServiceError> {
        let out = self
            .inner
            .edit_segment(pid, chapter, segment, target, expected)
            .map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `translate` — invalidates the project's cached reads.
    pub fn translate(&self, pid: &str) -> Result<(), ServiceError> {
        let out = self.inner.translate(pid).map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `translate_chapter` — invalidates the project's cached reads.
    pub fn translate_chapter(&self, pid: &str, index: u32) -> Result<(), ServiceError> {
        let out = self
            .inner
            .translate_chapter(pid, index)
            .map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `pause` — invalidates the project's cached reads.
    pub fn pause(&self, pid: &str) -> Result<(), ServiceError> {
        let out = self.inner.pause(pid).map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `resume` — invalidates the project's cached reads.
    pub fn resume(&self, pid: &str) -> Result<(), ServiceError> {
        let out = self.inner.resume(pid).map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `add_term` — invalidates the project's cached reads.
    pub fn add_term(&self, pid: &str, term: TermIn) -> Result<GlossaryTerm, ServiceError> {
        let out = self.inner.add_term(pid, term).map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `update_term` — invalidates the project's cached reads.
    pub fn update_term(
        &self,
        pid: &str,
        source: &str,
        term: TermIn,
    ) -> Result<GlossaryTerm, ServiceError> {
        let out = self
            .inner
            .update_term(pid, source, term)
            .map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `delete_term` — invalidates the project's cached reads.
    pub fn delete_term(&self, pid: &str, source: &str) -> Result<(), ServiceError> {
        let out = self
            .inner
            .delete_term(pid, source)
            .map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `resolve_conflict` — invalidates the project's cached reads.
    pub fn resolve_conflict(
        &self,
        pid: &str,
        conflict_id: i64,
        target: &str,
    ) -> Result<(), ServiceError> {
        let out = self
            .inner
            .resolve_conflict(pid, conflict_id, target)
            .map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `update_analysis` — invalidates the project's cached reads.
    pub fn update_analysis(
        &self,
        pid: &str,
        analysis: serde_json::Value,
    ) -> Result<(), ServiceError> {
        let out = self
            .inner
            .update_analysis(pid, analysis)
            .map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `update_digest` — invalidates the project's cached reads.
    pub fn update_digest(&self, pid: &str, chapter: u32, digest: &str) -> Result<(), ServiceError> {
        let out = self
            .inner
            .update_digest(pid, chapter, digest)
            .map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `update_chapter_title` — invalidates the project's cached reads.
    pub fn update_chapter_title(
        &self,
        pid: &str,
        chapter: u32,
        title: &str,
    ) -> Result<(), ServiceError> {
        let out = self
            .inner
            .update_chapter_title(pid, chapter, title)
            .map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `refresh_analysis` — invalidates the project's cached reads.
    pub fn refresh_analysis(&self, pid: &str) -> Result<(), ServiceError> {
        let out = self.inner.refresh_analysis(pid).map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project(pid);
        }
        out
    }

    /// `save_global_config_checked` — invalidates everything project-scoped.
    pub fn save_global_config_checked(
        &self,
        yaml: &str,
        revision: i64,
    ) -> Result<GlobalConfigOut, ServiceError> {
        let out = self
            .inner
            .save_global_config_checked(yaml, revision)
            .map_err(ServiceError::from);
        if out.is_ok() {
            self.invalidate_project("");
        }
        out
    }
}

impl std::ops::Deref for ServiceHandle {
    type Target = Service;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Active statuses — every status the engine sets while work runs.
pub const ACTIVE_STATUSES: &[&str] = &[
    "parsing",
    "queued",
    "preparing",
    "translating",
    "translating_subtitles",
    "reviewing",
    "autofixing",
    "postprocessing",
    "pausing",
    "curating",
];

/// True while the engine still has running work for the project.
pub fn is_project_busy(status: &str) -> bool {
    ACTIVE_STATUSES.contains(&status)
}
