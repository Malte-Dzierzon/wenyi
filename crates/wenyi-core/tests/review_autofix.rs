//! Autofix: shadow changes publish with optimistic checks; resume replays.

use std::collections::BTreeMap;
use std::path::PathBuf;
use wenyi_core::config;
use wenyi_core::glossary::GlossaryTerm;
use wenyi_core::llm::fake::FakeClient;
use wenyi_core::model::{BookId, ChapterId, ProjectId, SegmentId};
use wenyi_core::pipeline::Progress;
use wenyi_core::review::load_review_chapters;
use wenyi_core::review_autofix::run_autofix;
use wenyi_core::store;

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wenyi-core-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    store::db_path(&dir)
}

fn cfg() -> config::Config {
    config::load(
        "language: {source: en, target: zh}\nllm:\n  preset: fake\n  models:\n    default_strong: {provider: default, model: p}\n    default_cheap: {provider: default, model: f}\n",
    )
    .unwrap()
}

fn seed(name: &str) -> (PathBuf, ProjectId, BookId) {
    let db = temp_db(name);
    let conn = store::open(&db).unwrap();
    let project = ProjectId::new("p").unwrap();
    let book = BookId::new("b").unwrap();
    store::create_project(&conn, &project, "t").unwrap();
    store::create_book(&conn, &book, &project, 0, "B").unwrap();
    let chapter = ChapterId::new("c0").unwrap();
    store::create_chapter(&conn, &chapter, &book, 0, "C").unwrap();
    let pairs: Vec<(SegmentId, String)> = ["The old Wolf prowls.", "Moon rises."]
        .into_iter()
        .enumerate()
        .map(|(i, source)| (SegmentId::new(format!("s{i}")).unwrap(), source.to_string()))
        .collect();
    store::insert_segments(&conn, &chapter, &pairs).unwrap();
    for (sid, target) in [("s0", "老狼潜行。"), ("s1", "月升。")] {
        store::set_target(
            &conn,
            &SegmentId::new(sid).unwrap(),
            Some(target),
            "translate",
        )
        .unwrap();
    }
    store::set_chapter_status(&conn, &chapter, "done").unwrap();
    drop(conn);
    (db, project, book)
}

fn start_with_changes(conn: &rusqlite::Connection, project: &ProjectId, book: &BookId, id: &str) {
    store::start_review_run(conn, id, project, book, "digest", "{}").unwrap();
    let changes = serde_json::json!([
        {"chapter": 0, "index": 0, "suggested_target": "苍狼潜行。",
         "issue_keys": ["k1"], "review_result": "provisional"},
    ]);
    store::finish_review_run(
        conn,
        id,
        "completed",
        "clean_confirmed",
        r#"{"issue_count": 0, "change_count": 1, "round_count": 2}"#,
        "[]",
        &changes.to_string(),
        None,
    )
    .unwrap();
    // Reopen as the session left it (finish marks done; autofix reads the
    // stored issues/changes regardless of status).
}

fn plan_for(cfg: &config::Config) -> wenyi_core::i18n::policy::PolicyPlan {
    let groups = wenyi_core::i18n::policy::task_groups_full(
        "review", true, true, false, true, true, true, true,
    )
    .unwrap();
    let refs: Vec<&str> = groups.iter().map(|s| s as &str).collect();
    wenyi_core::i18n::policy::resolve_policy("en", "zh", "review", &refs, &cfg.honorific_strategy)
        .unwrap()
}

#[test]
fn shadow_change_publishes_formal_target() {
    let (db, project, book) = seed("publish");
    let conn = store::open(&db).unwrap();
    start_with_changes(&conn, &project, &book, "review-af");
    let cfg = cfg();
    let plan = plan_for(&cfg);
    let mut fake = FakeClient::new(None, None).unwrap();
    let mut events = Vec::new();
    let run = run_autofix(
        &mut fake,
        &conn,
        &project,
        &book,
        "review-af",
        0,
        &[GlossaryTerm::new("Wolf", "狼")],
        &serde_json::Map::new(),
        "",
        "",
        &BTreeMap::new(),
        "en",
        "zh",
        &plan,
        &cfg,
        &mut Progress::none(),
        &mut events,
    )
    .unwrap();
    assert!(events
        .iter()
        .any(|e| e["event"] == "review_autofix_planned"));
    assert!(events
        .iter()
        .any(|e| e["event"] == "review_autofix_finished"));
    let summary: serde_json::Value = serde_json::from_str(&run.summary_json).unwrap();
    assert_eq!(summary["autofix"]["applied_segment_count"], 1);
    assert_eq!(summary["autofix"]["status"], "completed");
    // Formal target now carries the shadow change.
    let chapters = store::list_chapters(&conn, &book, 0, 10).unwrap();
    let segments = store::list_segments(&conn, &chapters[0].id, 0, 10).unwrap();
    assert_eq!(segments[0].target.as_deref(), Some("苍狼潜行。"));
    // Index persisted before the write (recoverable record).
    let index = store::load_review_trace(&conn, "review-af", 0, "autofix/index.json")
        .unwrap()
        .unwrap();
    assert!(index.contains("\"status\":\"completed\""));
}

#[test]
fn changed_formal_target_fails_optimistic_check() {
    let (db, project, book) = seed("optimistic");
    let conn = store::open(&db).unwrap();
    start_with_changes(&conn, &project, &book, "review-af2");
    // A concurrent edit lands between index planning and publication:
    // pre-stage an `applying` index with the review-time `before`, then
    // change the formal target, then resume the index.
    let before_hash = wenyi_core::review_models::text_hash("老狼潜行。");
    let index = serde_json::json!({
        "version": 1, "review_id": "review-af2", "status": "applying",
        "reviewed_content_digest": "digest",
        "records": [{
            "record_id": "autofix-00001", "chapter": 0, "index": 0,
            "segment_ref": "ch0:text0:seg0", "origin": "change",
            "before": "老狼潜行。", "before_hash": before_hash,
            "after": "苍狼潜行。", "issue_keys": ["k1"], "issue_ids": [],
            "review_result": "provisional", "status": "planned",
        }],
        "locations": [{
            "chapter": 0, "index": 0, "segment_ref": "ch0:text0:seg0",
            "before": "老狼潜行。", "before_hash": before_hash,
            "target": "苍狼潜行。", "record_ids": ["autofix-00001"],
            "status": "pending", "alignment_status": "pending",
        }],
    });
    store::save_review_trace(
        &conn,
        "review-af2",
        0,
        "autofix/index.json",
        &index.to_string(),
    )
    .unwrap();
    let chapters = store::list_chapters(&conn, &book, 0, 10).unwrap();
    let segments = store::list_segments(&conn, &chapters[0].id, 0, 10).unwrap();
    store::set_target(&conn, &segments[0].id, Some("手动改。"), "manual").unwrap();
    let cfg = cfg();
    let plan = plan_for(&cfg);
    let mut fake = FakeClient::new(None, None).unwrap();
    let mut events = Vec::new();
    let run = run_autofix(
        &mut fake,
        &conn,
        &project,
        &book,
        "review-af2",
        0,
        &[GlossaryTerm::new("Wolf", "狼")],
        &serde_json::Map::new(),
        "",
        "",
        &BTreeMap::new(),
        "en",
        "zh",
        &plan,
        &cfg,
        &mut Progress::none(),
        &mut events,
    )
    .unwrap();
    let summary: serde_json::Value = serde_json::from_str(&run.summary_json).unwrap();
    assert_eq!(summary["autofix"]["status"], "partial");
    assert_eq!(summary["autofix"]["failed_record_count"], 1);
    // Manual edit preserved; shadow change not applied.
    let segments = store::list_segments(&conn, &chapters[0].id, 0, 10).unwrap();
    assert_eq!(segments[0].target.as_deref(), Some("手动改。"));
}

#[test]
fn applying_index_resumes_without_replanning() {
    let (db, project, book) = seed("resume");
    let conn = store::open(&db).unwrap();
    start_with_changes(&conn, &project, &book, "review-af3");
    let cfg = cfg();
    let plan = plan_for(&cfg);
    let mut fake = FakeClient::new(None, None).unwrap();
    let mut events = Vec::new();
    // First run publishes fully.
    run_autofix(
        &mut fake,
        &conn,
        &project,
        &book,
        "review-af3",
        0,
        &[GlossaryTerm::new("Wolf", "狼")],
        &serde_json::Map::new(),
        "",
        "",
        &BTreeMap::new(),
        "en",
        "zh",
        &plan,
        &cfg,
        &mut Progress::none(),
        &mut events,
    )
    .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["event"] == "review_autofix_planned")
            .count(),
        1
    );
    // Second run hits the finished index: idempotent finish, no replan.
    let mut events2 = Vec::new();
    let run = run_autofix(
        &mut fake,
        &conn,
        &project,
        &book,
        "review-af3",
        0,
        &[GlossaryTerm::new("Wolf", "狼")],
        &serde_json::Map::new(),
        "",
        "",
        &BTreeMap::new(),
        "en",
        "zh",
        &plan,
        &cfg,
        &mut Progress::none(),
        &mut events2,
    )
    .unwrap();
    assert!(!events2
        .iter()
        .any(|e| e["event"] == "review_autofix_planned"));
    let summary: serde_json::Value = serde_json::from_str(&run.summary_json).unwrap();
    assert_eq!(summary["autofix"]["applied_segment_count"], 1);
    let chapters = load_review_chapters(&conn, &book).unwrap();
    assert_eq!(chapters[0].segments.len(), 2);
}
