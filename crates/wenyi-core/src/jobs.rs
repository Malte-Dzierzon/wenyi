//! Bounded jobs runtime. Oracle: `wenyi_desktop/local_runtime.py`.
//!
//! Two bounded queues (workflow/export), fixed consumers, per-chapter
//! lanes + curator exception, cooperative cancellation at checkpoints,
//! startup/periodic recovery, low-frequency curator sweep, ≤4 Hz progress
//! events. SQLite owns durable state; this module owns task slots.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use crate::model::{JobId, ProjectId, RunId};
use crate::store;
use crate::{Error, Result};

/// Supported task names (oracle `TASK_NAMES`).
pub const TASK_NAMES: [&str; 9] = [
    "run_parse",
    "run_prepare",
    "run_translation",
    "run_chapter_translation",
    "run_review",
    "run_srt",
    "run_export",
    "run_glossary_curator",
    "run_refresh_analysis",
];

/// Parallel workflow lanes (chapter translations run concurrently).
/// Four lanes: three chapter translations plus headroom so one book-wide
/// job (or a slow chapter) never starves the remaining chapters.
pub const WORKFLOW_CONSUMERS: usize = 4;
/// Parallel export lanes.
pub const EXPORT_CONSUMERS: usize = 2;
/// Bounded queue depth (backpressure: full queues reject admission).
pub const QUEUE_DEPTH: usize = 128;
/// Workflow watchdog (24 h); export watchdog (1 h).
pub const WORKFLOW_TIMEOUT: Duration = Duration::from_secs(86400);
/// Export watchdog.
pub const EXPORT_TIMEOUT: Duration = Duration::from_secs(3600);
/// Stale-job grace before periodic recovery interrupts (2 min).
pub const RECOVERY_GRACE: Duration = Duration::from_secs(120);
/// Periodic recovery sweep interval.
pub const RECOVERY_INTERVAL: Duration = Duration::from_secs(30);
/// Curator sweep interval (one pass ≈ 20 terms).
pub const CURATOR_INTERVAL: Duration = Duration::from_secs(300);
/// Bounded shutdown flush for in-flight checkpoint writes.
pub const SHUTDOWN_FLUSH: Duration = Duration::from_secs(15);
/// Progress event fan-out capacity (coarse ≤4 Hz updates).
pub const PROGRESS_CHANNEL: usize = 64;

/// One unit of scheduled work.
#[derive(Debug, Clone)]
pub struct WorkItem {
    /// Task name (`run_*`).
    pub name: String,
    /// Owning project.
    pub project_id: ProjectId,
    /// Durable run identity.
    pub run_id: RunId,
    /// Job row identity.
    pub job_id: JobId,
    /// Task parameters.
    pub params: BTreeMap<String, serde_json::Value>,
}

/// Task execution outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutcome {
    /// Completed; durable status set to `done`.
    Done,
    /// Stopped at a checkpoint; resumable.
    Interrupted(String),
    /// Failed with a user-visible message.
    Failed(String),
}

/// Executable task body: checkpoint-cooperative async work.
pub type TaskFn = Arc<dyn Fn(WorkItem, Cancel) -> futures::TaskFuture + Send + Sync>;

/// Minimal boxed-future alias without new dependencies.
pub mod futures {
    use std::future::Future;
    use std::pin::Pin;
    /// Boxed task future.
    pub type TaskFuture = Pin<Box<dyn Future<Output = super::TaskOutcome> + Send + 'static>>;
}

/// Cooperative cancellation token (checkpoint polling).
#[derive(Debug, Clone, Default)]
pub struct Cancel {
    flag: Arc<std::sync::atomic::AtomicBool>,
}

impl Cancel {
    /// Signal cancellation.
    pub fn cancel(&self) {
        self.flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// True once cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Typed progress event (coarse; UI queries the store for details).
#[derive(Debug, Clone)]
pub struct ProgressEvent {
    /// Owning project.
    pub project_id: ProjectId,
    /// Job run identity.
    pub run_id: RunId,
    /// Event kind (`started`, `progress`, `done`, `failed`, `interrupted`).
    pub kind: String,
    /// Completed work units.
    pub done: i64,
    /// Total work units (0 when unknown).
    pub total: i64,
    /// Human message.
    pub message: String,
}

/// Runtime configuration (queue depths, workers, intervals).
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Workflow queue depth.
    pub workflow_depth: usize,
    /// Export queue depth.
    pub export_depth: usize,
    /// Workflow consumers.
    pub workflow_consumers: usize,
    /// Export consumers.
    pub export_consumers: usize,
    /// Recovery sweep interval.
    pub recovery_interval: Duration,
    /// Curator sweep interval (None disables).
    pub curator_interval: Option<Duration>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            workflow_depth: QUEUE_DEPTH,
            export_depth: QUEUE_DEPTH,
            workflow_consumers: WORKFLOW_CONSUMERS,
            export_consumers: EXPORT_CONSUMERS,
            recovery_interval: RECOVERY_INTERVAL,
            curator_interval: Some(CURATOR_INTERVAL),
        }
    }
}

struct ConsumerState {
    submitted: HashSet<String>,
    cancels: HashMap<String, Cancel>,
}

/// Bounded jobs runtime over a workspace database.
pub struct JobsRuntime {
    db_path: PathBuf,
    config: RuntimeConfig,
    workflow_tx: mpsc::Sender<Option<WorkItem>>,
    export_tx: mpsc::Sender<Option<WorkItem>>,
    workflow_rx: Arc<tokio::sync::Mutex<Option<mpsc::Receiver<Option<WorkItem>>>>>,
    export_rx: Arc<tokio::sync::Mutex<Option<mpsc::Receiver<Option<WorkItem>>>>>,
    state: Arc<Mutex<ConsumerState>>,
    stopping: watch::Sender<bool>,
    stopped: Arc<tokio::sync::RwLock<bool>>,
    accepting: std::sync::atomic::AtomicBool,
    progress: broadcast::Sender<ProgressEvent>,
    tasks: BTreeMap<String, TaskFn>,
    curator: Option<TaskFn>,
}

impl JobsRuntime {
    /// Build over a workspace DB path (opened per operation; short
    /// transactions only, never held across task execution).
    pub fn new(db_path: PathBuf, config: RuntimeConfig) -> Self {
        let (workflow_tx, workflow_rx) = mpsc::channel(config.workflow_depth);
        let (export_tx, export_rx) = mpsc::channel(config.export_depth);
        let (progress, _) = broadcast::channel(PROGRESS_CHANNEL);
        Self {
            db_path,
            config,
            workflow_tx,
            export_tx,
            workflow_rx: Arc::new(tokio::sync::Mutex::new(Some(workflow_rx))),
            export_rx: Arc::new(tokio::sync::Mutex::new(Some(export_rx))),
            state: Arc::new(Mutex::new(ConsumerState {
                submitted: HashSet::new(),
                cancels: HashMap::new(),
            })),
            stopping: watch::channel(false).0,
            stopped: Arc::new(tokio::sync::RwLock::new(false)),
            accepting: std::sync::atomic::AtomicBool::new(false),
            progress,
            tasks: BTreeMap::new(),
            curator: None,
        }
    }

    /// Flip the accepting flag without starting consumers (queue-depth tests).
    #[doc(hidden)]
    pub fn accepting_for_tests(&self) {
        self.accepting
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Register a task implementation.
    pub fn register(&mut self, name: &str, task: TaskFn) {
        if TASK_NAMES.contains(&name) {
            self.tasks.insert(name.to_string(), task);
        }
    }

    /// Register the curator sweep body.
    pub fn register_curator(&mut self, task: TaskFn) {
        self.curator = Some(task);
    }

    /// Subscribe to progress events.
    pub fn subscribe(&self) -> broadcast::Receiver<ProgressEvent> {
        self.progress.subscribe()
    }

    /// True when no work is submitted (all consumed results removed).
    pub fn idle(&self) -> bool {
        let state = self.state.lock().expect("jobs state");
        state.submitted.is_empty()
    }

    /// Admit one durable job and enqueue it (oracle `admit_job` +
    /// `enqueue`). Rejects unknown tasks, missing durable records,
    /// lane conflicts, and full queues.
    pub fn admit(
        &self,
        project_id: &ProjectId,
        kind: &str,
        name: &str,
        job_id: &JobId,
        run_id: &RunId,
        params: BTreeMap<String, serde_json::Value>,
    ) -> Result<WorkItem> {
        if !self.accepting.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Error::Store(
                "The local worker is not accepting tasks.".to_string(),
            ));
        }
        if !TASK_NAMES.contains(&name) {
            return Err(Error::Config(format!("Unsupported local task: {name}")));
        }
        let conn = store::open(&self.db_path)?;
        let job = store::get_job_by_run(&conn, run_id)?.ok_or_else(|| {
            Error::Config("The local task has no matching durable record.".to_string())
        })?;
        if job.project_id.as_str() != project_id.as_str() {
            return Err(Error::Config(
                "The local task has no matching durable record.".to_string(),
            ));
        }
        // ONE critical section: duplicate check, lane check, enqueue, and
        // bookkeeping share a single lock. The section is sync-only (no
        // await), so this leaf lock cannot deadlock; it closes the race
        // where a periodic sweep interrupts a just-admitted run between the
        // check and the insert. On a full queue the inserts roll back.
        let mut state = self.state.lock().expect("jobs state");
        if state.submitted.contains(run_id.as_str()) || job.status != "queued" {
            return Ok(WorkItem {
                name: name.to_string(),
                project_id: project_id.clone(),
                run_id: run_id.clone(),
                job_id: job_id.clone(),
                params,
            });
        }
        Self::check_lanes(&conn, project_id, kind, &params, run_id)?;
        let queue = if name == "run_export" {
            &self.export_tx
        } else {
            &self.workflow_tx
        };
        let item = WorkItem {
            name: name.to_string(),
            project_id: project_id.clone(),
            run_id: run_id.clone(),
            job_id: job_id.clone(),
            params,
        };
        let cancel = Cancel::default();
        state.submitted.insert(run_id.as_str().to_string());
        state.cancels.insert(run_id.as_str().to_string(), cancel);
        if queue.try_send(Some(item.clone())).is_err() {
            state.submitted.remove(run_id.as_str());
            state.cancels.remove(run_id.as_str());
            return Err(Error::Store(format!(
                "The {name} queue is full; try again later."
            )));
        }
        Ok(item)
    }

    /// Admission lanes (oracle `admit_job`): curator never conflicts;
    /// chapter jobs conflict per chapter or against book-wide work;
    /// book-wide jobs need an otherwise idle project.
    fn check_lanes(
        conn: &rusqlite::Connection,
        project_id: &ProjectId,
        kind: &str,
        params: &BTreeMap<String, serde_json::Value>,
        self_run: &RunId,
    ) -> Result<()> {
        let active: Vec<_> = store::list_active_jobs_with_params(conn, project_id)?
            .into_iter()
            .filter(|(job, _)| job.run_id.as_str() != self_run.as_str())
            .collect();
        if kind == "glossary_curator" {
            if active.iter().any(|(job, _)| job.kind == "glossary_curator") {
                return Err(Error::Store(
                    "A curator pass is already running.".to_string(),
                ));
            }
            return Ok(());
        }
        if kind == "chapter_translation" {
            let chapter = params.get("chapter_index").and_then(|v| v.as_i64());
            if let Some(chapter) = chapter {
                for (row, params_json) in &active {
                    if row.kind == "glossary_curator" {
                        continue;
                    }
                    let lane = Self::params_chapter(params_json);
                    if row.kind != "chapter_translation" || lane == Some(chapter) {
                        return Err(Error::Store(
                            "Project already has a conflicting running task.".to_string(),
                        ));
                    }
                }
                return Ok(());
            }
        }
        if active.iter().any(|(job, _)| job.kind != "glossary_curator") {
            return Err(Error::Store(
                "Project already has a running task.".to_string(),
            ));
        }
        Ok(())
    }

    fn params_chapter(params_json: &str) -> Option<i64> {
        let params: serde_json::Value =
            serde_json::from_str(params_json).unwrap_or(serde_json::Value::Null);
        params.get("chapter_index").and_then(|v| v.as_i64())
    }

    /// Cancel one run cooperatively (checkpoint boundary) and durably.
    ///
    /// Signals the stored [`Cancel`] flag so a running task observes
    /// `is_cancelled()` at its next checkpoint, then performs the durable
    /// `queued`/`running` → `cancelled` transition.
    pub fn cancel(&self, run_id: &RunId) -> Result<bool> {
        if let Some(cancel) = self
            .state
            .lock()
            .expect("jobs state")
            .cancels
            .get(run_id.as_str())
            .cloned()
        {
            cancel.cancel();
        }
        let conn = store::open(&self.db_path)?;
        store::transition_job(&conn, run_id, &["queued", "running"], "cancelled")
    }

    /// Start consumers + sweeps after startup recovery.
    pub async fn start(self: Arc<Self>) -> Result<Vec<JoinHandle<()>>> {
        self.recover_startup()?;
        self.accepting
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let mut handles = Vec::new();
        let workflow_rx = self.workflow_rx.lock().await.take().expect("start once");
        let export_rx = self.export_rx.lock().await.take().expect("start once");
        let workflow_rx = Arc::new(tokio::sync::Mutex::new(workflow_rx));
        let export_rx = Arc::new(tokio::sync::Mutex::new(export_rx));
        for _ in 0..self.config.workflow_consumers {
            let runtime = Arc::clone(&self);
            let rx = Arc::clone(&workflow_rx);
            handles.push(tokio::spawn(async move {
                runtime.consume(rx, WORKFLOW_TIMEOUT).await;
            }));
        }
        for _ in 0..self.config.export_consumers {
            let runtime = Arc::clone(&self);
            let rx = Arc::clone(&export_rx);
            handles.push(tokio::spawn(async move {
                runtime.consume(rx, EXPORT_TIMEOUT).await;
            }));
        }
        {
            let runtime = Arc::clone(&self);
            let interval = self.config.recovery_interval;
            let stopping = self.stopping.subscribe();
            handles.push(tokio::spawn(async move {
                runtime.recover_periodically(stopping, interval).await;
            }));
        }
        if let Some(interval) = self.config.curator_interval {
            let runtime = Arc::clone(&self);
            let stopping = self.stopping.subscribe();
            handles.push(tokio::spawn(async move {
                runtime.curate_periodically(stopping, interval).await;
            }));
        }
        Ok(handles)
    }

    async fn consume(
        &self,
        rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Option<WorkItem>>>>,
        timeout: Duration,
    ) {
        loop {
            let item = { rx.lock().await.recv().await };
            let Some(item) = item else { return };
            let Some(work) = item else { return };
            let run_key = work.run_id.as_str().to_string();
            if !self.accepting.load(std::sync::atomic::Ordering::SeqCst) {
                self.interrupt(&work.run_id, "Desktop is shutting down.");
                let mut state = self.state.lock().expect("jobs state");
                state.submitted.remove(&run_key);
                state.cancels.remove(&run_key);
                continue;
            }
            let Some(task) = self.tasks.get(&work.name).cloned() else {
                self.interrupt(
                    &work.run_id,
                    &format!("Unsupported local task: {}", work.name),
                );
                let mut state = self.state.lock().expect("jobs state");
                state.submitted.remove(&run_key);
                state.cancels.remove(&run_key);
                continue;
            };
            // Mark running (idempotent transition from queued).
            if let Ok(conn) = store::open(&self.db_path) {
                let _ = store::transition_job(&conn, &work.run_id, &["queued"], "running");
            }
            self.publish(&work, "started", 0, 0, String::new());
            let cancel = self
                .state
                .lock()
                .expect("jobs state")
                .cancels
                .get(&run_key)
                .cloned()
                .unwrap_or_default();
            let future = task(work.clone(), cancel.clone());
            let watchdog = tokio::time::timeout(timeout, future);
            let outcome = watchdog.await.map_err(|_| "timeout".to_string());
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(_) => TaskOutcome::Interrupted("Task watchdog expired.".to_string()),
            };
            let outcome = if cancel.is_cancelled() {
                match outcome {
                    TaskOutcome::Done => {
                        TaskOutcome::Interrupted("The local task was cancelled.".to_string())
                    }
                    other => other,
                }
            } else {
                outcome
            };
            self.finish(&work, outcome);
            let mut state = self.state.lock().expect("jobs state");
            state.submitted.remove(&run_key);
            state.cancels.remove(&run_key);
        }
    }

    fn finish(&self, work: &WorkItem, outcome: TaskOutcome) {
        let (status, kind, message) = match &outcome {
            TaskOutcome::Done => ("done", "done", String::new()),
            TaskOutcome::Interrupted(reason) => ("interrupted", "interrupted", reason.clone()),
            TaskOutcome::Failed(reason) => ("error", "failed", reason.clone()),
        };
        if let Ok(conn) = store::open(&self.db_path) {
            let _ = store::finish_job(
                &conn,
                &work.run_id,
                status,
                (!message.is_empty()).then_some(message.as_str()),
            );
            let _ = store::append_job_event(
                &conn,
                &work.job_id,
                kind,
                &serde_json::json!({"message": message}).to_string(),
            );
        }
        self.publish(work, kind, 0, 0, message);
    }

    fn publish(&self, work: &WorkItem, kind: &str, done: i64, total: i64, message: String) {
        let _ = self.progress.send(ProgressEvent {
            project_id: work.project_id.clone(),
            run_id: work.run_id.clone(),
            kind: kind.to_string(),
            done,
            total,
            message,
        });
    }

    /// Interrupt one run (queued/running → interrupted/error).
    pub fn interrupt(&self, run_id: &RunId, reason: &str) {
        let Ok(conn) = store::open(&self.db_path) else {
            return;
        };
        let Ok(Some(job)) = store::get_job_by_run(&conn, run_id) else {
            return;
        };
        if job.status != "queued" && job.status != "running" {
            return;
        }
        // A live worker must never be declared dead: only interrupt runs
        // this runtime does not currently own.
        if self
            .state
            .lock()
            .expect("jobs state")
            .submitted
            .contains(run_id.as_str())
        {
            return;
        }
        let status = if job.kind == "export" {
            "error"
        } else {
            "interrupted"
        };
        let _ = store::finish_job(&conn, run_id, status, Some(reason));
        let _ = store::append_job_event(
            &conn,
            &job.id,
            status,
            &serde_json::json!({"reason": reason}).to_string(),
        );
    }

    /// Startup recovery: interrupt every unfinished run (previous worker
    /// is definitionally gone; checkpoints replay).
    pub fn recover_startup(&self) -> Result<()> {
        let conn = store::open(&self.db_path)?;
        // Clear owned runs first (fresh runtime owns nothing).
        {
            let mut state = self.state.lock().expect("jobs state");
            state.submitted.clear();
            state.cancels.clear();
        }
        for job in store::list_unfinished_jobs(&conn)? {
            // Bypass the ownership guard: a fresh runtime owns nothing, and
            // every unfinished run belongs to a dead worker.
            let status = if job.kind == "export" {
                "error"
            } else {
                "interrupted"
            };
            let _ = store::finish_job(
                &conn,
                &job.run_id,
                status,
                Some("The previous local worker stopped; resume saved work."),
            );
        }
        Ok(())
    }

    async fn recover_periodically(&self, mut stopping: watch::Receiver<bool>, interval: Duration) {
        loop {
            tokio::select! {
                _ = stopping.wait_for(|stop| *stop) => return,
                _ = tokio::time::sleep(interval) => {}
            }
            // Stale-only sweep: skip runs written within RECOVERY_GRACE so a
            // just-admitted run is never mistaken for an orphan. The announce
            // (`submitted` guard in `interrupt`) is the second layer; the
            // grace window is the first.
            let now = store::now_ms();
            let grace = i64::try_from(RECOVERY_GRACE.as_millis()).unwrap_or(i64::MAX);
            let unfinished = store::open(&self.db_path)
                .and_then(|conn| store::list_unfinished_jobs(&conn))
                .unwrap_or_default();
            for job in unfinished {
                if now.wrapping_sub(job.updated_at) < grace {
                    continue;
                }
                self.interrupt(
                    &job.run_id,
                    "The previous local worker stopped; resume saved work.",
                );
            }
        }
    }

    async fn curate_periodically(&self, mut stopping: watch::Receiver<bool>, interval: Duration) {
        loop {
            tokio::select! {
                _ = stopping.wait_for(|stop| *stop) => return,
                _ = tokio::time::sleep(interval) => {}
            }
            // Low-frequency safety sweep: admit one curator pass per project
            // that has glossary terms and no active curator. Normal work is
            // event-driven (translation reindexes dirty books); the sweep
            // only recovers missed dirty work. The body must be registered
            // via `register("run_glossary_curator", ..)` before `start()`;
            // admission fails safe (already-active/full) and retries next
            // sweep.
            if !self.tasks.contains_key("run_glossary_curator") {
                continue;
            }
            let projects = store::open(&self.db_path)
                .and_then(|conn| store::list_projects(&conn))
                .unwrap_or_default();
            for project in projects {
                let has_terms = store::open(&self.db_path)
                    .and_then(|conn| crate::glossary::all_terms_counts(&conn, &project.id))
                    .unwrap_or(0)
                    > 0;
                if !has_terms {
                    continue;
                }
                let run_id = match crate::model::RunId::new(format!(
                    "curator-{}-{}",
                    project.id.as_str(),
                    store::now_ms()
                )) {
                    Ok(id) => id,
                    Err(_) => continue,
                };
                let job_id = match crate::model::JobId::new(format!("job-{}", run_id.as_str())) {
                    Ok(id) => id,
                    Err(_) => continue,
                };
                let enqueued = store::open(&self.db_path).and_then(|conn| {
                    store::enqueue_job(
                        &conn,
                        &job_id,
                        &project.id,
                        "glossary_curator",
                        &run_id,
                        "{}",
                    )
                });
                if enqueued.is_err() {
                    continue;
                }
                let mut params = BTreeMap::new();
                params.insert(
                    "db_path".to_string(),
                    serde_json::Value::String(self.db_path.to_string_lossy().into_owned()),
                );
                let _ = self.admit(
                    &project.id,
                    "glossary_curator",
                    "run_glossary_curator",
                    &job_id,
                    &run_id,
                    params,
                );
            }
        }
    }

    /// Graceful stop: drain queues, bounded flush, wake consumers.
    ///
    /// Sends one `None` sentinel per consumer on each channel so `consume`
    /// loops observe shutdown even when their queue is empty, bounding the
    /// wait with `SHUTDOWN_FLUSH` per channel. Queued-but-unstarted runs are
    /// interrupted first; in-flight tasks checkpoint cooperatively.
    pub async fn stop(&self) {
        self.accepting
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = self.stopping.send(true);
        // Interrupting queued (never-started) runs is immediate.
        let unfinished = store::open(&self.db_path)
            .and_then(|conn| store::list_unfinished_jobs(&conn))
            .unwrap_or_default();
        for job in unfinished {
            if job.status == "queued" {
                self.interrupt(&job.run_id, "Desktop is shutting down.");
            }
        }
        // Wake every consumer: one sentinel each, bounded by SHUTDOWN_FLUSH.
        for _ in 0..self.config.workflow_consumers {
            let send = self.workflow_tx.send(None);
            if tokio::time::timeout(SHUTDOWN_FLUSH, send).await.is_err() {
                break;
            }
        }
        for _ in 0..self.config.export_consumers {
            let send = self.export_tx.send(None);
            if tokio::time::timeout(SHUTDOWN_FLUSH, send).await.is_err() {
                break;
            }
        }
        *self.stopped.write().await = true;
    }
}
