//! Review session: clean pass, shadow fix round, resume skip.

use std::collections::BTreeMap;
use std::path::PathBuf;
use wenyi_core::config;
use wenyi_core::glossary::GlossaryTerm;
use wenyi_core::llm::compat::Message;
use wenyi_core::llm::fake::{FakeClient, FakeHandler};
use wenyi_core::model::{BookId, ChapterId, ProjectId, SegmentId};
use wenyi_core::pipeline::Progress;
use wenyi_core::review::{
    formal_content_digest, load_review_chapters, review_gate, run_session, ReviewChapter,
    ReviewRunner,
};
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

fn seed_two_segments(member: &str) -> (PathBuf, ProjectId, BookId) {
    let db = temp_db(member);
    let conn = store::open(&db).unwrap();
    let project = ProjectId::new("p").unwrap();
    let book = BookId::new("b").unwrap();
    store::create_project(&conn, &project, "t").unwrap();
    store::create_book(&conn, &book, &project, 0, "B").unwrap();
    let chapter = ChapterId::new("c0").unwrap();
    store::create_chapter(&conn, &chapter, &book, 0, "C").unwrap();
    let pairs: Vec<(SegmentId, String)> = [("The old Wolf prowls.", "s0"), ("Moon rises.", "s1")]
        .into_iter()
        .map(|(source, id)| (SegmentId::new(id).unwrap(), source.to_string()))
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

fn clean_handler() -> FakeHandler {
    Box::new(|messages: &[Message], _: &str, _: bool| {
        let system = messages.first().map(|m| m.content.as_str()).unwrap_or("");
        if system.contains("rigorous translation reviewer") {
            let n = messages
                .last()
                .map(|m| {
                    m.content
                        .lines()
                        .filter(|line| {
                            line.starts_with('[')
                                && line.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
                        })
                        .count()
                })
                .unwrap_or(0);
            return serde_json::json!({"issues": [], "reviewed_segments": n, "complete": true})
                .to_string();
        }
        serde_json::json!({"action": "final", "decisions": [], "new_issues": [], "complete": true})
            .to_string()
    })
}

#[test]
fn clean_session_completes_without_formal_writes() {
    let (db, project, book) = seed_two_segments("review-clean");
    let conn = store::open(&db).unwrap();
    assert!(review_gate(&conn, &book).unwrap().is_empty());
    let chapters: Vec<ReviewChapter> = load_review_chapters(&conn, &book).unwrap();
    assert_eq!(chapters[0].segments.len(), 2);
    let digest = formal_content_digest(&conn, &book).unwrap();
    assert_eq!(digest.len(), 64);
    store::start_review_run(&conn, "review-clean", &project, &book, &digest, "{}").unwrap();
    let cfg = cfg();
    let mut fake = FakeClient::new(Some(clean_handler()), None).unwrap();
    let mut runner = ReviewRunner {
        config: &cfg,
        client: &mut fake,
        src: "en".to_string(),
        tgt: "zh".to_string(),
    };
    let mut events = Vec::new();
    let run = run_session(
        &mut runner,
        &conn,
        &project,
        &book,
        "review-clean",
        &chapters,
        &[GlossaryTerm::new("Wolf", "狼")],
        &serde_json::Map::new(),
        &BTreeMap::new(),
        "",
        "",
        &mut Progress::none(),
        &mut events,
    )
    .unwrap();
    assert_eq!(run.status, "completed");
    assert_eq!(run.termination, "clean_confirmed");
    assert_eq!(run.issues_json, "[]");
    assert_eq!(run.changes_json, "[]");
    assert!(events.iter().any(|e| e["event"] == "review_finished"));
    // Formal targets untouched.
    let chapters = store::list_chapters(&conn, &book, 0, 10).unwrap();
    let segments = store::list_segments(&conn, &chapters[0].id, 0, 10).unwrap();
    assert_eq!(segments[0].target.as_deref(), Some("老狼潜行。"));
}

#[test]
fn scan_issue_drives_shadow_fix_and_reports_change() {
    let (db, project, book) = seed_two_segments("review-fix");
    let conn = store::open(&db).unwrap();
    let chapters: Vec<ReviewChapter> = load_review_chapters(&conn, &book).unwrap();
    let digest = formal_content_digest(&conn, &book).unwrap();
    store::start_review_run(&conn, "review-fix", &project, &book, &digest, "{}").unwrap();
    let cfg = cfg();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let handler: FakeHandler = Box::new(move |messages: &[Message], _: &str, _: bool| {
        let system = messages.first().map(|m| m.content.as_str()).unwrap_or("");
        if system.contains("rigorous translation reviewer") {
            let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                return serde_json::json!({
                    "issues": [{"index": 0, "type": "mistranslation",
                                "detail": "Wrong beast", "suggestion": "Use 蒂狼"}],
                    "reviewed_segments": 2, "complete": true,
                })
                .to_string();
            }
            return serde_json::json!({"issues": [], "reviewed_segments": 2, "complete": true})
                .to_string();
        }
        if system.contains("cautious revision editor") {
            return serde_json::json!({
                "segment_ref": "ch0:text0:seg0", "before_hash": wenyi_core::review_models::text_hash("老狼潜行。"),
                "issue_ids": ["r1-review-00001"],
                "replacement": "苍狼潜行。", "complete": true,
            })
            .to_string();
        }
        // Verify loop: confirm the candidate.
        serde_json::json!({
            "action": "final",
            "decisions": [{"candidate_id": "r1-ch0-base0-candidate0", "verdict": "confirmed",
                           "detail": "Wrong beast", "suggestion": "Use 苍狼",
                           "consistency": {}, "evidence_refs": []}],
            "new_issues": [], "complete": true,
        })
        .to_string()
    });
    let mut fake = FakeClient::new(Some(handler), None).unwrap();
    let mut runner = ReviewRunner {
        config: &cfg,
        client: &mut fake,
        src: "en".to_string(),
        tgt: "zh".to_string(),
    };
    let mut events = Vec::new();
    let run = run_session(
        &mut runner,
        &conn,
        &project,
        &book,
        "review-fix",
        &chapters,
        &[GlossaryTerm::new("Wolf", "狼")],
        &serde_json::Map::new(),
        &BTreeMap::new(),
        "",
        "",
        &mut Progress::none(),
        &mut events,
    )
    .unwrap();
    assert_eq!(run.status, "completed", "termination={}", run.termination);
    let changes: Vec<serde_json::Value> = serde_json::from_str(&run.changes_json).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["suggested_target"], "苍狼潜行。");
    // Formal target still untouched (shadow only).
    let chapters = store::list_chapters(&conn, &book, 0, 10).unwrap();
    let segments = store::list_segments(&conn, &chapters[0].id, 0, 10).unwrap();
    assert_eq!(segments[0].target.as_deref(), Some("老狼潜行。"));
    assert!(events
        .iter()
        .any(|e| e["event"] == "review_fix_round_finished"));
}

#[test]
fn resume_reuses_cached_chunks_without_new_scan_calls() {
    let (db, project, book) = seed_two_segments("review-resume");
    let conn = store::open(&db).unwrap();
    let chapters: Vec<ReviewChapter> = load_review_chapters(&conn, &book).unwrap();
    let digest = formal_content_digest(&conn, &book).unwrap();
    store::start_review_run(&conn, "review-resume", &project, &book, &digest, "{}").unwrap();
    let cfg = cfg();
    let mut fake = FakeClient::new(Some(clean_handler()), None).unwrap();
    let mut runner = ReviewRunner {
        config: &cfg,
        client: &mut fake,
        src: "en".to_string(),
        tgt: "zh".to_string(),
    };
    let mut events = Vec::new();
    run_session(
        &mut runner,
        &conn,
        &project,
        &book,
        "review-resume",
        &chapters,
        &[GlossaryTerm::new("Wolf", "狼")],
        &serde_json::Map::new(),
        &BTreeMap::new(),
        "",
        "",
        &mut Progress::none(),
        &mut events,
    )
    .unwrap();
    let first_calls = runner.client.call_count();
    // Resume via checkpoint: the completed scan replays from chunk caches.
    // Simulate a fixer-stage crash by rewinding the checkpoint to scan_done
    // of round 1 (already persisted) — rerun must add zero scan calls.
    let checkpoint = store::load_review_checkpoint(&conn, "review-resume")
        .unwrap()
        .unwrap();
    assert!(checkpoint.contains("scan_done") || checkpoint.contains("round_done"));
    drop(runner);
    drop(fake);
    let mut fake = FakeClient::new(Some(clean_handler()), None).unwrap();
    let mut runner = ReviewRunner {
        config: &cfg,
        client: &mut fake,
        src: "en".to_string(),
        tgt: "zh".to_string(),
    };
    let mut events2 = Vec::new();
    // Force a scan_done resume by resetting the checkpoint phase.
    let mut payload: serde_json::Value = serde_json::from_str(&checkpoint).unwrap();
    payload["phase"] = serde_json::json!("scan_done");
    payload["next_round"] = serde_json::json!(1);
    store::save_review_checkpoint(&conn, "review-resume", &payload.to_string()).unwrap();
    let run = run_session(
        &mut runner,
        &conn,
        &project,
        &book,
        "review-resume",
        &chapters,
        &[GlossaryTerm::new("Wolf", "狼")],
        &serde_json::Map::new(),
        &BTreeMap::new(),
        "",
        "",
        &mut Progress::none(),
        &mut events2,
    )
    .unwrap();
    assert_eq!(run.status, "completed");
    assert!(events2.iter().any(|e| e["event"] == "review_scan_skipped"));
    assert_eq!(
        runner.client.call_count(),
        0,
        "scan replay must not call the model"
    );
    assert!(first_calls > 0);
}
