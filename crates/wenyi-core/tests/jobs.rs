//! Jobs runtime: admission lanes, bounds, cancellation, recovery.
//!
//! Oracle: `wenyi_desktop/local_runtime.py` + `local_backend.admit_job`.
//! Each test owns a scratch workspace DB; tasks are scripted bodies.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use wenyi_core::jobs::{
    futures::TaskFuture, Cancel, JobsRuntime, RuntimeConfig, TaskOutcome, WorkItem, TASK_NAMES,
};
use wenyi_core::model::{JobId, ProjectId, RunId};
use wenyi_core::store;

fn scratch_db(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wenyi-jobs-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    store::db_path(&dir)
}

fn seed_job(db: &std::path::Path, project: &str, kind: &str, job: &str, run: &str, params: &str) {
    let conn = store::open(db).unwrap();
    let pid = ProjectId::new(project).unwrap();
    if store::get_project(&conn, &pid).unwrap().is_none() {
        store::create_project(&conn, &pid, project).unwrap();
    }
    store::enqueue_job(
        &conn,
        &JobId::new(job).unwrap(),
        &pid,
        kind,
        &RunId::new(run).unwrap(),
        params,
    )
    .unwrap();
}

fn done_task() -> Arc<dyn Fn(WorkItem, Cancel) -> TaskFuture + Send + Sync> {
    Arc::new(|_work, _cancel| Box::pin(async { TaskOutcome::Done }))
}

fn runtime(db: std::path::PathBuf) -> (Arc<JobsRuntime>, RuntimeConfig) {
    let mut rt = JobsRuntime::new(
        db,
        RuntimeConfig {
            workflow_depth: 4,
            export_depth: 4,
            workflow_consumers: 1,
            export_consumers: 1,
            recovery_interval: Duration::from_secs(3600),
            curator_interval: None,
        },
    );
    rt.register("run_translation", done_task());
    rt.register("run_chapter_translation", done_task());
    rt.register("run_export", done_task());
    rt.register("run_glossary_curator", done_task());
    let config = RuntimeConfig {
        workflow_depth: 4,
        export_depth: 4,
        workflow_consumers: 1,
        export_consumers: 1,
        recovery_interval: Duration::from_secs(3600),
        curator_interval: None,
    };
    (Arc::new(rt), config)
}

fn params(pairs: &[(&str, i64)]) -> BTreeMap<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), serde_json::Value::from(*v)))
        .collect()
}

#[test]
fn task_names_match_oracle() {
    let mut names = TASK_NAMES.to_vec();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "run_chapter_translation",
            "run_export",
            "run_glossary_curator",
            "run_parse",
            "run_prepare",
            "run_refresh_analysis",
            "run_review",
            "run_srt",
            "run_translation",
        ]
    );
}

#[tokio::test]
async fn admit_runs_task_and_marks_done() {
    let db = scratch_db("done");
    let (rt, _) = runtime(db.clone());
    let handles = Arc::clone(&rt).start().await.unwrap();
    seed_job(&db, "p1", "translation", "j1", "r1", "{}");
    let item = rt
        .admit(
            &ProjectId::new("p1").unwrap(),
            "translation",
            "run_translation",
            &JobId::new("j1").unwrap(),
            &RunId::new("r1").unwrap(),
            BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(item.run_id.as_str(), "r1");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if rt.idle() {
                // Give the consumer a beat to finish the DB write.
                tokio::time::sleep(Duration::from_millis(50)).await;
                if rt.idle() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let conn = store::open(&db).unwrap();
    let job = store::get_job_by_run(&conn, &RunId::new("r1").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(job.status, "done");
    rt.stop().await;
    for handle in handles {
        handle.abort();
    }
}

#[tokio::test]
async fn chapter_lanes_conflict_per_chapter() {
    let db = scratch_db("lanes");
    let (rt, _) = runtime(db.clone());
    rt.accepting_for_tests();
    // No consumers: jobs stay queued so lane conflicts are deterministic.
    // Seed each row immediately before its admit (the oracle creates the
    // row inside admission, so unadmitted siblings must not exist yet).
    seed_job(
        &db,
        "p1",
        "chapter_translation",
        "j1",
        "r1",
        r#"{"chapter_index": 3}"#,
    );
    rt.admit(
        &ProjectId::new("p1").unwrap(),
        "chapter_translation",
        "run_chapter_translation",
        &JobId::new("j1").unwrap(),
        &RunId::new("r1").unwrap(),
        params(&[("chapter_index", 3)]),
    )
    .unwrap();
    // Same chapter conflicts.
    seed_job(
        &db,
        "p1",
        "chapter_translation",
        "j2",
        "r2",
        r#"{"chapter_index": 3}"#,
    );
    let conflict = rt.admit(
        &ProjectId::new("p1").unwrap(),
        "chapter_translation",
        "run_chapter_translation",
        &JobId::new("j2").unwrap(),
        &RunId::new("r2").unwrap(),
        params(&[("chapter_index", 3)]),
    );
    assert!(conflict.is_err(), "same chapter must conflict");
    // Different chapter is fine.
    seed_job(
        &db,
        "p1",
        "chapter_translation",
        "j3",
        "r3",
        r#"{"chapter_index": 4}"#,
    );
    rt.admit(
        &ProjectId::new("p1").unwrap(),
        "chapter_translation",
        "run_chapter_translation",
        &JobId::new("j3").unwrap(),
        &RunId::new("r3").unwrap(),
        params(&[("chapter_index", 4)]),
    )
    .unwrap();
    rt.stop().await;
}

#[tokio::test]
async fn book_wide_needs_idle_project_but_curator_is_exempt() {
    let db = scratch_db("bookwide");
    let (rt, _) = runtime(db.clone());
    rt.accepting_for_tests();
    seed_job(&db, "p1", "translation", "j1", "r1", "{}");
    seed_job(&db, "p1", "glossary_curator", "jc", "rc", "{}");
    rt.admit(
        &ProjectId::new("p1").unwrap(),
        "glossary_curator",
        "run_glossary_curator",
        &JobId::new("jc").unwrap(),
        &RunId::new("rc").unwrap(),
        BTreeMap::new(),
    )
    .unwrap();
    // Book-wide work proceeds alongside the curator.
    rt.admit(
        &ProjectId::new("p1").unwrap(),
        "translation",
        "run_translation",
        &JobId::new("j1").unwrap(),
        &RunId::new("r1").unwrap(),
        BTreeMap::new(),
    )
    .unwrap();
    // A second curator pass conflicts with the first.
    seed_job(&db, "p1", "glossary_curator", "jc2", "rc2", "{}");
    let conflict = rt.admit(
        &ProjectId::new("p1").unwrap(),
        "glossary_curator",
        "run_glossary_curator",
        &JobId::new("jc2").unwrap(),
        &RunId::new("rc2").unwrap(),
        BTreeMap::new(),
    );
    assert!(conflict.is_err(), "second curator must conflict");
    rt.stop().await;
}

#[tokio::test]
async fn full_queue_rejects_with_backpressure() {
    let db = scratch_db("full");
    let mut rt = JobsRuntime::new(
        db.clone(),
        RuntimeConfig {
            workflow_depth: 1,
            export_depth: 1,
            workflow_consumers: 1,
            export_consumers: 1,
            recovery_interval: Duration::from_secs(3600),
            curator_interval: None,
        },
    );
    // No consumers started: the queue fills and the second admit fails.
    rt.register("run_translation", done_task());
    let rt = Arc::new(rt);
    rt.accepting_for_tests();
    seed_job(&db, "p1", "translation", "j1", "r1", "{}");
    rt.admit(
        &ProjectId::new("p1").unwrap(),
        "translation",
        "run_translation",
        &JobId::new("j1").unwrap(),
        &RunId::new("r1").unwrap(),
        BTreeMap::new(),
    )
    .unwrap();
    // Same-lane second job would conflict first; use a fresh project.
    seed_job(&db, "p2", "translation", "j3", "r3", "{}");
    let full = rt.admit(
        &ProjectId::new("p2").unwrap(),
        "translation",
        "run_translation",
        &JobId::new("j3").unwrap(),
        &RunId::new("r3").unwrap(),
        BTreeMap::new(),
    );
    assert!(full.is_err(), "full queue must reject");
    assert!(full.unwrap_err().to_string().contains("full"));
}

#[tokio::test]
async fn cancel_transitions_queued_job() {
    let db = scratch_db("cancel");
    let (rt, _) = runtime(db.clone());
    let _handles = Arc::clone(&rt).start().await.unwrap();
    seed_job(&db, "p1", "translation", "j1", "r1", "{}");
    assert!(rt.cancel(&RunId::new("r1").unwrap()).unwrap());
    let conn = store::open(&db).unwrap();
    let job = store::get_job_by_run(&conn, &RunId::new("r1").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(job.status, "cancelled");
    // Second cancel is a no-op (terminal already).
    assert!(!rt.cancel(&RunId::new("r1").unwrap()).unwrap());
    rt.stop().await;
}

#[tokio::test]
async fn startup_recovery_interrupts_orphaned_runs() {
    let db = scratch_db("recover");
    seed_job(&db, "p1", "translation", "j1", "r1", "{}");
    seed_job(&db, "p1", "export", "j2", "r2", "{}");
    // Simulate a dead worker: flip both to running behind the runtime.
    let conn = store::open(&db).unwrap();
    assert!(
        store::transition_job(&conn, &RunId::new("r1").unwrap(), &["queued"], "running").unwrap()
    );
    assert!(
        store::transition_job(&conn, &RunId::new("r2").unwrap(), &["queued"], "running").unwrap()
    );
    drop(conn);
    let (rt, _) = runtime(db.clone());
    rt.recover_startup().unwrap();
    let conn = store::open(&db).unwrap();
    let workflow = store::get_job_by_run(&conn, &RunId::new("r1").unwrap())
        .unwrap()
        .unwrap();
    let export = store::get_job_by_run(&conn, &RunId::new("r2").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(workflow.status, "interrupted");
    assert_eq!(export.status, "error");
}

#[tokio::test]
async fn usage_records_exactly_once_per_batch_key() {
    let db = scratch_db("usage");
    seed_job(&db, "p1", "translation", "j1", "r1", "{}");
    let conn = store::open(&db).unwrap();
    let job_id = JobId::new("j1").unwrap();
    let sample = store::UsageRecord {
        batch_key: "r1:batch-0",
        tier: "strong",
        stage: "translation.body",
        provider: "deepseek",
        model: "deepseek-flash",
        input_tokens: 100,
        output_tokens: 50,
    };
    assert!(store::record_usage(&conn, &job_id, &sample).unwrap());
    // Replay (retry/resume) is a no-op returning false.
    assert!(!store::record_usage(&conn, &job_id, &sample).unwrap());
}

#[tokio::test]
async fn progress_events_flow_to_subscribers() {
    let db = scratch_db("progress");
    let (rt, _) = runtime(db.clone());
    let mut events = rt.subscribe();
    let _handles = Arc::clone(&rt).start().await.unwrap();
    seed_job(&db, "p1", "translation", "j1", "r1", "{}");
    rt.admit(
        &ProjectId::new("p1").unwrap(),
        "translation",
        "run_translation",
        &JobId::new("j1").unwrap(),
        &RunId::new("r1").unwrap(),
        BTreeMap::new(),
    )
    .unwrap();
    let mut kinds = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while kinds.len() < 2 && tokio::time::Instant::now() < deadline {
        if let Ok(Ok(event)) = tokio::time::timeout(Duration::from_millis(200), events.recv()).await
        {
            kinds.push(event.kind);
        }
    }
    assert!(kinds.contains(&"started".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"done".to_string()), "{kinds:?}");
    rt.stop().await;
}

#[tokio::test]
async fn stop_drains_idle_consumers() {
    let db = scratch_db("stop-drain");
    let (rt, _) = runtime(db.clone());
    let handles = Arc::clone(&rt).start().await.unwrap();
    assert!(rt.idle());
    // No work queued: stop must still wake every consumer via sentinels.
    tokio::time::timeout(Duration::from_secs(5), rt.stop())
        .await
        .unwrap();
    assert!(rt.idle());
    tokio::time::timeout(Duration::from_secs(5), async {
        for handle in handles {
            handle.await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancel_signals_running_task() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let db = scratch_db("cancel-running");
    let mut rt = JobsRuntime::new(
        db.clone(),
        RuntimeConfig {
            workflow_depth: 4,
            export_depth: 4,
            workflow_consumers: 1,
            export_consumers: 1,
            recovery_interval: Duration::from_secs(3600),
            curator_interval: None,
        },
    );
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&saw_cancel);
    rt.register(
        "run_translation",
        Arc::new(move |_work: WorkItem, cancel: Cancel| -> TaskFuture {
            let flag = Arc::clone(&flag);
            Box::pin(async move {
                for _ in 0..200 {
                    if cancel.is_cancelled() {
                        flag.store(true, Ordering::SeqCst);
                        return TaskOutcome::Interrupted("cancelled".to_string());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                TaskOutcome::Done
            })
        }),
    );
    let rt = Arc::new(rt);
    let handles = Arc::clone(&rt).start().await.unwrap();
    seed_job(&db, "p1", "translation", "j1", "r1", "{}");
    rt.admit(
        &ProjectId::new("p1").unwrap(),
        "translation",
        "run_translation",
        &JobId::new("j1").unwrap(),
        &RunId::new("r1").unwrap(),
        BTreeMap::new(),
    )
    .unwrap();
    // Wait until the task is running, then cancel: the flag must flip and
    // the run must reach a terminal state promptly.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let conn = store::open(&db).unwrap();
            let job = store::get_job_by_run(&conn, &RunId::new("r1").unwrap())
                .unwrap()
                .unwrap();
            if job.status == "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(rt.cancel(&RunId::new("r1").unwrap()).unwrap());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if saw_cancel.load(Ordering::SeqCst) && rt.idle() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let conn = store::open(&db).unwrap();
    let job = store::get_job_by_run(&conn, &RunId::new("r1").unwrap())
        .unwrap()
        .unwrap();
    assert!(saw_cancel.load(Ordering::SeqCst));
    assert_eq!(job.status, "cancelled");
    rt.stop().await;
    for handle in handles {
        handle.abort();
    }
}

#[tokio::test]
async fn admit_survives_immediate_recovery_sweep() {
    let db = scratch_db("admit-sweep");
    let mut rt = JobsRuntime::new(
        db.clone(),
        RuntimeConfig {
            workflow_depth: 4,
            export_depth: 4,
            workflow_consumers: 1,
            export_consumers: 1,
            recovery_interval: Duration::from_millis(10),
            curator_interval: None,
        },
    );
    rt.register("run_translation", done_task());
    let rt = Arc::new(rt);
    let handles = Arc::clone(&rt).start().await.unwrap();
    // Sweep with a 10 ms interval fires while the admit below is fresh; the
    // grace window plus the single-critical-section admit must keep the
    // just-admitted run out of the orphan path.
    tokio::time::sleep(Duration::from_millis(15)).await;
    seed_job(&db, "p1", "translation", "j1", "r1", "{}");
    rt.admit(
        &ProjectId::new("p1").unwrap(),
        "translation",
        "run_translation",
        &JobId::new("j1").unwrap(),
        &RunId::new("r1").unwrap(),
        BTreeMap::new(),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if rt.idle() {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if rt.idle() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let conn = store::open(&db).unwrap();
    let job = store::get_job_by_run(&conn, &RunId::new("r1").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(job.status, "done");
    rt.stop().await;
    for handle in handles {
        handle.abort();
    }
}
