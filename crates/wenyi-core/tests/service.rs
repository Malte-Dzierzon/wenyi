//! Service integration: open/list/create/delete, import, edits, history,
//! exports, events on the jobs level.

use std::path::PathBuf;
use std::sync::Arc;

use wenyi_core::service::dto::*;
use wenyi_core::service::Service;
use wenyi_core::{store, Error};

fn temp_workspace(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wenyi-service-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_service(name: &str) -> (PathBuf, Service) {
    let dir = temp_workspace(name);
    let service = Service::open(&dir).unwrap();
    (dir, service)
}

#[test]
fn open_list_create_delete_project_round_trip() {
    let (_dir, service) = open_service("crud");
    assert!(service.list_projects().unwrap().is_empty());
    let detail = service
        .create_project("Demo", "en", "zh", "Hello world.\n".as_bytes(), "demo.txt")
        .unwrap();
    assert_eq!(detail.project.name, "Demo");
    assert_eq!(detail.chapter_count, 1);
    let pid = detail.project.id.clone();
    let listed = service.list_projects().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, pid);
    let fetched = service.get_project(&pid).unwrap();
    assert_eq!(fetched.total_word_count, 2);
    service.delete_project(&pid).unwrap();
    assert!(service.list_projects().unwrap().is_empty());
    let missing = service.get_project(&pid).unwrap_err();
    assert!(matches!(missing, Error::NotFound { .. }));
}

#[test]
fn import_txt_creates_chapters() {
    let (_dir, service) = open_service("import");
    let body = "# Chapter One\n\nFirst paragraph.\n\n# Chapter Two\n\nSecond paragraph.\n";
    let detail = service
        .create_project("Book", "en", "zh", body.as_bytes(), "book.md")
        .unwrap();
    let pid = detail.project.id.clone();
    let chapters = service.list_chapters(&pid).unwrap();
    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[0].title, "Chapter One");
    let chapter = service.get_chapter(&pid, 1).unwrap();
    assert_eq!(chapter.title, "Chapter Two");
    assert!(!chapter.segments.is_empty());
}

#[test]
fn import_unsupported_formats_error() {
    let (_dir, service) = open_service("unsupported");
    for (name, bytes) in [("doc.docx", &b"fake"[..]), ("scan.pdf", &b"fake"[..])] {
        let error = service
            .create_project("Bad", "en", "zh", bytes, name)
            .unwrap_err();
        assert!(
            matches!(error, Error::Unsupported(_)),
            "unexpected: {error:?}"
        );
    }
}

#[test]
fn edit_segment_history_and_conflict() {
    let (_dir, service) = open_service("edit");
    let detail = service
        .create_project("Edit", "en", "zh", "Alpha.\n".as_bytes(), "a.txt")
        .unwrap();
    let pid = detail.project.id.clone();
    // Pending -> set without expectation.
    service.edit_segment(&pid, 0, 0, "阿尔法。", None).unwrap();
    let history = service.segment_history(&pid, 0, 0).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].after.as_deref(), Some("阿尔法。"));
    // Stale expectation -> Conflict.
    let conflict = service
        .edit_segment(&pid, 0, 0, "贝塔。", Some("stale"))
        .unwrap_err();
    assert!(matches!(conflict, Error::Conflict(_)), "{conflict:?}");
    // Correct expectation -> applies.
    service
        .edit_segment(&pid, 0, 0, "贝塔。", Some("阿尔法。"))
        .unwrap();
    let history = service.segment_history(&pid, 0, 0).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].before.as_deref(), Some("阿尔法。"));
    assert_eq!(history[1].after.as_deref(), Some("贝塔。"));
}

#[test]
fn export_record_round_trip_and_bytes() {
    let (dir, service) = open_service("export");
    let detail = service
        .create_project("Exp", "en", "zh", "Hello.\n".as_bytes(), "e.txt")
        .unwrap();
    let pid = detail.project.id.clone();
    let conn = store::open(&store::db_path(&dir)).unwrap();
    let pid_typed = wenyi_core::model::ProjectId::new(pid.clone()).unwrap();
    let book = store::list_books(&conn, &pid_typed).unwrap().remove(0);
    let record = store::create_export(&conn, "x1", &pid_typed, &book.id, "txt", "{}").unwrap();
    assert_eq!(record.status, "queued");
    store::set_export_status(&conn, "x1", "done", Some("/tmp/out.txt"), Some(4), None).unwrap();
    drop(conn);
    let exports = service.list_exports(&pid).unwrap();
    assert_eq!(exports.len(), 1);
    let out: ExportOut = exports.into_iter().next().unwrap();
    assert_eq!(out.id, "x1");
    assert_eq!(out.status, "done");
    std::fs::write("/tmp/out.txt", b"data").unwrap();
    // Point the record at a real file for the bytes check.
    let conn = store::open(&store::db_path(&dir)).unwrap();
    let content_path = dir.join("content.txt");
    std::fs::write(&content_path, b"payload").unwrap();
    store::set_export_status(
        &conn,
        "x1",
        "done",
        Some(&content_path.to_string_lossy()),
        Some(7),
        None,
    )
    .unwrap();
    drop(conn);
    let bytes = service.export_bytes("x1").unwrap();
    assert_eq!(bytes, b"payload");
}

#[test]
fn global_config_round_trip_and_credentials() {
    let (_dir, service) = open_service("config");
    let initial = service.get_global_config().unwrap();
    assert!(initial.yaml.is_empty() || !initial.yaml.is_empty());
    let yaml = "language: {source: en, target: zh}\nllm: {preset: fake}\n";
    let saved = service.save_global_config(yaml).unwrap();
    assert_eq!(saved.yaml, yaml);
    let statuses = service.credential_statuses();
    assert!(!statuses.is_empty());
    // `default` connection exists in the fake preset.
    service.save_credential("default", "secret").unwrap();
    let statuses = service.credential_statuses();
    let entry = statuses.iter().find(|s| s.connection == "default").unwrap();
    assert!(entry.configured);
    assert_eq!(entry.source, "session");
    let caps = service.capabilities();
    assert!(caps.input_formats.contains(&"txt".to_string()));
    assert!(caps.output_formats.contains(&"docx".to_string()));
}

#[test]
fn glossary_term_lifecycle() {
    let (_dir, service) = open_service("glossary");
    let detail = service
        .create_project("G", "en", "zh", "Wolf prowls.\n".as_bytes(), "g.txt")
        .unwrap();
    let pid = detail.project.id.clone();
    let term = TermIn {
        source: "Wolf".to_string(),
        target: "狼".to_string(),
        ..Default::default()
    };
    let created = service.add_term(&pid, term.clone()).unwrap();
    assert_eq!(created.target, "狼");
    assert_eq!(service.list_terms(&pid).unwrap().len(), 1);
    // Conflicting upsert -> conflict row.
    let conflict_term = TermIn {
        source: "Wolf".to_string(),
        target: "苍狼".to_string(),
        ..Default::default()
    };
    service.add_term(&pid, conflict_term).unwrap();
    let conflicts = service.list_conflicts(&pid).unwrap();
    assert_eq!(conflicts.len(), 1);
    service
        .resolve_conflict(&pid, conflicts[0].id, "苍狼")
        .unwrap();
    assert!(service.list_conflicts(&pid).unwrap().is_empty());
    service.delete_term(&pid, "Wolf").unwrap();
    assert!(service.list_terms(&pid).unwrap().is_empty());
}

#[tokio::test]
async fn jobs_event_subscription_flows() {
    use std::collections::BTreeMap;
    use std::time::Duration;
    use wenyi_core::jobs::{JobsRuntime, RuntimeConfig};
    use wenyi_core::model::{JobId, ProjectId, RunId};

    let dir = temp_workspace("events");
    let db = store::db_path(&dir);
    let conn = store::open(&db).unwrap();
    let pid = ProjectId::new("p1").unwrap();
    store::create_project(&conn, &pid, "p1").unwrap();
    drop(conn);
    let mut runtime = JobsRuntime::new(
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
    runtime.register(
        "run_translation",
        Arc::new(|_, _| {
            Box::pin(async { wenyi_core::jobs::TaskOutcome::Done })
                as wenyi_core::jobs::futures::TaskFuture
        }),
    );
    let runtime = Arc::new(runtime);
    let mut events = runtime.subscribe();
    let handles = Arc::clone(&runtime).start().await.unwrap();
    let conn = store::open(&db).unwrap();
    let job_id = JobId::new("j1").unwrap();
    let run_id = RunId::new("r1").unwrap();
    store::enqueue_job(&conn, &job_id, &pid, "translation", &run_id, "{}").unwrap();
    drop(conn);
    runtime
        .admit(
            &pid,
            "translation",
            "run_translation",
            &job_id,
            &run_id,
            BTreeMap::new(),
        )
        .unwrap();
    let mut kinds = Vec::new();
    for _ in 0..2 {
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        kinds.push(event.kind.clone());
        // Engine event mapping keeps run identity.
        let engine = wenyi_core::service::dto::EngineEvent::from(&event);
        assert_eq!(engine.run_id, "r1");
    }
    assert!(kinds.contains(&"started".to_string()));
    assert!(kinds.contains(&"done".to_string()));
    runtime.stop().await;
    for handle in handles {
        handle.abort();
    }
}

#[test]
fn stats_report_events_reviews_round_trip() {
    let (_dir, service) = open_service("stats");
    let detail = service
        .create_project(
            "Stats",
            "en",
            "de",
            "# Ch\n\nHello world.\n".as_bytes(),
            "s.md",
        )
        .unwrap();
    let pid = detail.project.id.clone();
    // Fresh project: empty ledgers, empty report, no runs, no events.
    let stats = service.get_stats(&pid).unwrap();
    assert!(stats.usage.is_object());
    assert!(stats.timing.is_object());
    let report = service.get_report(&pid).unwrap();
    assert_eq!(
        report
            .get("summary")
            .and_then(|s| s.get("chapters_total"))
            .and_then(|v| v.as_i64()),
        Some(1)
    );
    assert!(service.list_review_runs(&pid).unwrap().is_empty());
    assert!(service.list_events(&pid).unwrap().is_empty());
    assert!(service.regenerate_report(&pid).is_ok());
    // Translated title round-trip.
    service.update_chapter_title(&pid, 0, "Kapitel").unwrap();
    let chapters = service.list_chapters(&pid).unwrap();
    assert_eq!(chapters[0].title_translated.as_deref(), Some("Kapitel"));
}

#[test]
fn list_projects_fills_languages_format_title_status_date() {
    let (_dir, service) = open_service("listrows");
    let detail = service
        .create_project("Rows", "en", "de", "Hello world.\n".as_bytes(), "r.txt")
        .unwrap();
    let listed = service.list_projects().unwrap();
    assert_eq!(listed.len(), 1);
    let row = &listed[0];
    assert_eq!(row.id, detail.project.id);
    assert_eq!(row.source_lang.as_deref(), Some("en"));
    assert_eq!(row.target_lang.as_deref(), Some("de"));
    assert_eq!(row.fmt.as_deref(), Some("txt"));
    assert!(row.title.as_deref().is_some_and(|t| !t.is_empty()));
    assert!(!row.status.is_empty());
    assert!(row.initialized);
    let date = row.created_at.as_deref().unwrap_or("");
    assert_eq!(date.len(), 10, "expected YYYY-MM-DD, got {date}");
    assert_eq!(&date[4..5], "-");
}

#[test]
fn subscribe_outside_runtime_does_not_panic() {
    let (_dir, service) = open_service("subscribenort");
    // Called on a plain thread: must return a receiver, never panic.
    let handle = std::thread::spawn(move || service.subscribe());
    let _rx = handle.join().expect("subscribe panicked");
}

#[test]
fn chapter_statistics_cache_backfills_and_updates() {
    let (_dir, service) = open_service("statscache");
    let detail = service
        .create_project(
            "Cache",
            "en",
            "de",
            "Hello brave world.\n".as_bytes(),
            "c.txt",
        )
        .unwrap();
    let pid = detail.project.id.clone();
    let chapters = service.list_chapters(&pid).unwrap();
    assert_eq!(chapters[0].word_count, 3);
    assert_eq!(chapters[0].target_word_count, 0);
    service
        .edit_segment(&pid, 0, 0, "Hallo mutige Welt.", None)
        .unwrap();
    let chapters = service.list_chapters(&pid).unwrap();
    assert_eq!(chapters[0].target_word_count, 3);
}

#[test]
fn global_settings_validate_defaults_revision_round_trip() {
    let (_dir, service) = open_service("settings2");
    let defaults = Service::get_global_defaults();
    assert!(!defaults.yaml.is_empty());
    assert!(!defaults.default_template.is_empty());
    // Dry-run validates without persisting.
    let yaml = "language: {source: en, target: zh}\nllm: {preset: fake}\n";
    let checked = service.validate_global_config(yaml).unwrap();
    assert_eq!(checked.yaml, yaml);
    assert!(service.get_global_config().unwrap().yaml.is_empty());
    // Revision starts at 1; a save bumps to 2; stale baseline -> Conflict.
    assert_eq!(service.get_global_config().unwrap().revision, 1);
    let saved = service.save_global_config(yaml).unwrap();
    assert_eq!(saved.revision, 2);
    let stale = service.save_global_config_checked(yaml, 1);
    assert!(matches!(stale, Err(Error::Conflict(_))));
    let fresh = service.save_global_config_checked(yaml, 2).unwrap();
    assert_eq!(fresh.revision, 3);
    // Operations come from the compiled registry (routing + tier defaults).
    let caps = service.capabilities();
    assert!(!caps.operations.is_empty());
    assert!(caps
        .operations
        .iter()
        .any(|op| op.id == "glossary.curate" && op.tier == "cheap"));
    assert!(caps
        .operations
        .iter()
        .any(|op| op.id == "translation.body" && op.tier == "strong"));
}

#[test]
fn curator_status_start_stop_round_trip() {
    let (_dir, service) = open_service("curator");
    let detail = service
        .create_project("C", "en", "zh", "Wolf prowls.\n".as_bytes(), "c.txt")
        .unwrap();
    let pid = detail.project.id.clone();
    service.accepting_for_tests();
    let idle = service.curator_status(&pid).unwrap();
    assert!(!idle.running);
    assert!(idle.run_id.is_none());
    assert_eq!(idle.terms_total, 0);
    service
        .add_term(
            &pid,
            TermIn {
                source: "Wolf".into(),
                target: "狼".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let started = service.curator_start(&pid).unwrap();
    assert!(!started.run_id.is_empty());
    let busy = service.curator_status(&pid).unwrap();
    assert!(busy.running);
    assert_eq!(busy.run_id.as_deref(), Some(started.run_id.as_str()));
    assert_eq!(busy.terms_total, 1);
    // Second start while active -> Conflict (409 equivalent).
    assert!(matches!(
        service.curator_start(&pid),
        Err(Error::Conflict(_))
    ));
    service.curator_stop(&pid).unwrap();
    let conn = store::open(&store::db_path(&_dir)).unwrap();
    let state = store::load_curator_state(
        &conn,
        &wenyi_core::model::ProjectId::new(pid.clone()).unwrap(),
        "en",
        "zh",
    )
    .unwrap();
    assert_eq!(
        state.get("stop_requested"),
        Some(&serde_json::Value::Bool(true))
    );
}

#[tokio::test]
async fn four_chapter_jobs_admit_in_parallel() {
    use std::collections::BTreeMap;
    use wenyi_core::jobs::JobsRuntime;
    use wenyi_core::model::{JobId, ProjectId, RunId};
    let (_dir, service) = open_service("lanes4");
    let detail = service
        .create_project(
            "L",
            "en",
            "zh",
            "One.\n\nTwo.\n\nThree.\n\nFour.\n\nFive.\n".as_bytes(),
            "l.txt",
        )
        .unwrap();
    let pid = detail.project.id.clone();
    service.accepting_for_tests();
    // Four distinct chapters admit without lane conflicts (needs the four
    // consumers + per-chapter lanes both to hold).
    for chapter in 0..4 {
        service.translate_chapter(&pid, chapter).unwrap();
    }
    // Same chapter twice still conflicts.
    let dup = service.translate_chapter(&pid, 0);
    assert!(dup.is_err());
    // Runtime-level admission of 4 distinct chapter lanes.
    let db = store::db_path(&_dir);
    let conn = store::open(&db).unwrap();
    let rt = JobsRuntime::new(db.clone(), wenyi_core::jobs::RuntimeConfig::default());
    rt.accepting_for_tests();
    let project = ProjectId::new("p4").unwrap();
    if store::get_project(&conn, &project).unwrap().is_none() {
        store::create_project(&conn, &project, "p4").unwrap();
    }
    for (i, (job, run)) in [("j0", "r0"), ("j1", "r1"), ("j2", "r2"), ("j3", "r3")]
        .iter()
        .enumerate()
    {
        store::enqueue_job(
            &conn,
            &JobId::new(*job).unwrap(),
            &project,
            "chapter_translation",
            &RunId::new(*run).unwrap(),
            &format!(r#"{{"chapter_index": {i}}}"#),
        )
        .unwrap();
    }
    let mut admitted = Vec::new();
    for (job, run, chapter) in [
        ("j0", "r0", 0),
        ("j1", "r1", 1),
        ("j2", "r2", 2),
        ("j3", "r3", 3),
    ] {
        let mut params = BTreeMap::new();
        params.insert(
            "chapter_index".to_string(),
            serde_json::Value::from(chapter),
        );
        admitted.push(
            rt.admit(
                &project,
                "chapter_translation",
                "run_chapter_translation",
                &JobId::new(job).unwrap(),
                &RunId::new(run).unwrap(),
                params,
            )
            .unwrap(),
        );
    }
    assert_eq!(admitted.len(), 4);
    assert_eq!(wenyi_core::jobs::WORKFLOW_CONSUMERS, 4);
}
