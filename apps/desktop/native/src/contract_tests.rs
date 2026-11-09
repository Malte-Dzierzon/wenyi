//! Backend-contract tests: Python-shaped JSON still deserializes into the
//! native client's structs, and status/audience gates match the backend.
//!
//! Reference shapes: `packages/backend/wenyi_backend/schemas.py`,
//! `dal.RUNNING_PROJECT_STATUSES`, `job_service.TASK_STATUSES`.
//! Hermetic: no network, no sidecar spawn, no filesystem.

use crate::client::{
    ACTIVE_STATUSES, ChapterSummary, Client, EventOut, ExportOut, Project, ProjectDetail,
    ReviewRun, WorkflowOut, is_project_busy,
};
use crate::nav::Audience;

fn project_json() -> serde_json::Value {
    serde_json::json!({
        "id": "proj-1",
        "name": "Demo",
        "title": "Demo Title",
        "fmt": "epub",
        "source_lang": "en",
        "target_lang": "zh",
        "status": "created",
        "error": null,
        "initialized": true,
        "strategy": {"steps": {}},
        "created_at": "2026-01-01T00:00:00Z",
    })
}

#[test]
fn project_deserializes_python_shape() {
    let p: Project = serde_json::from_value(project_json()).unwrap();
    assert_eq!(p.id, "proj-1");
    assert_eq!(p.fmt.as_deref(), Some("epub"));
    assert!(p.initialized);
    assert_eq!(p.created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
}

#[test]
fn project_detail_deserializes_with_defaults() {
    let mut v = project_json();
    v["book_title"] = serde_json::json!("Demo Title");
    v["chapter_count"] = serde_json::json!(3);
    v["total_word_count"] = serde_json::json!(1200);
    v["done_chapters"] = serde_json::json!(1);
    let d: ProjectDetail = serde_json::from_value(v).unwrap();
    assert_eq!(d.project.id, "proj-1");
    assert_eq!(d.book_title.as_deref(), Some("Demo Title"));
    assert_eq!(d.chapter_count, 3);
    assert_eq!(d.total_word_count, 1200);
    assert_eq!(d.done_chapters, 1);
}

#[test]
fn project_detail_missing_optional_counts_default_to_zero() {
    let d: ProjectDetail = serde_json::from_value(project_json()).unwrap();
    assert_eq!(d.chapter_count, 0);
    assert_eq!(d.total_word_count, 0);
    assert_eq!(d.done_chapters, 0);
    assert_eq!(d.book_title, None);
}

#[test]
fn chapter_summary_deserializes() {
    let c: ChapterSummary = serde_json::from_str(
        r#"{"index": 2, "title": "Ch 3", "title_translated": "第三章",
            "status": "done", "word_count": 500, "target_word_count": 480,
            "review_issue_count": 1, "review_status": "reviewed"}"#,
    )
    .unwrap();
    assert_eq!(c.index, 2);
    assert_eq!(c.title_translated.as_deref(), Some("第三章"));
    assert_eq!(c.review_issue_count, 1);
}

#[test]
fn review_run_slim_list_shape_deserializes() {
    // Mirrors `_review_run_summary` in `routers/review.py`: identity + counts
    // only, empty issues/changes/result bodies.
    let run: ReviewRun = serde_json::from_str(
        r#"{"id": "review-abc", "review_id": "review-abc", "status": "done",
            "created_at": "2026-01-02T00:00:00Z",
            "issues": [], "changes": [], "autofix": {},
            "summary": {"issue_count": 4, "change_count": 2}, "result": {}}"#,
    )
    .unwrap();
    assert_eq!(run.id, "review-abc");
    assert!(run.issues.is_empty());
    assert!(run.changes.is_empty());
    assert_eq!(run.summary["issue_count"], 4);
    assert_eq!(run.summary["change_count"], 2);
}

#[test]
fn review_run_full_shape_with_items_deserializes() {
    let run: ReviewRun = serde_json::from_str(
        r#"{"id": "review-abc", "review_id": "review-abc", "status": "done",
            "created_at": null, "issues": [{"id": "i1"}], "changes": [],
            "autofix": {}, "summary": {}, "result": {"status": "done"},
            "items": [{"id": "ri-1", "kind": "issue", "status": "pending",
                       "location": null, "evidence": []}]}"#,
    )
    .unwrap();
    assert_eq!(run.issues.len(), 1);
    assert_eq!(run.items.len(), 1);
    assert_eq!(run.items[0].id, "ri-1");
    assert_eq!(run.result["status"], "done");
}

#[test]
fn event_out_deserializes_type_rename() {
    let e: EventOut = serde_json::from_str(
        r#"{"id": 7, "type": "chapter_done",
             "payload": {"chapter": 0, "title": "Intro"},
             "created_at": "2026-01-03T00:00:00Z"}"#,
    )
    .unwrap();
    assert_eq!(e.id, 7);
    assert_eq!(e.event_type, "chapter_done");
    assert_eq!(e.payload["title"], "Intro");
}

#[test]
fn workflow_out_deserializes() {
    let w: WorkflowOut = serde_json::from_str(
        r#"{"source": "auto", "kind": "book", "status": "translating",
             "run_id": "run-1", "review_id": null,
             "stages": [{"id": "translate", "label": "Translate"}],
             "progress": null, "running_chapters": [0, 1]}"#,
    )
    .unwrap();
    assert_eq!(w.kind, "book");
    assert_eq!(w.stages.len(), 1);
    assert!(w.stages[0].enabled);
    assert_eq!(w.running_chapters, vec![0, 1]);
}

#[test]
fn export_out_deserializes() {
    let e: ExportOut = serde_json::from_str(
        r#"{"id": 3, "project_id": "proj-1", "format": "epub",
             "status": "done", "path": "/tmp/out.epub", "size": 42,
             "error": null, "created_at": null}"#,
    )
    .unwrap();
    assert_eq!(e.format, "epub");
    assert_eq!(e.size, Some(42));
}

#[test]
fn busy_covers_all_task_statuses() {
    // Values of `TASK_STATUSES` in `job_service.py`.
    for s in ["parsing", "preparing", "translating", "reviewing", "curating"] {
        assert!(is_project_busy(s), "{s} should be busy");
    }
}

#[test]
fn busy_covers_translating_subtitles_and_curating() {
    assert!(is_project_busy("translating_subtitles"));
    assert!(is_project_busy("curating"));
}

#[test]
fn busy_rejects_terminal_and_empty() {
    for s in ["created", "done", "failed", "", "paused"] {
        assert!(!is_project_busy(s), "{s} should not be busy");
    }
}

#[test]
fn active_statuses_cover_running_project_statuses() {
    // `RUNNING_PROJECT_STATUSES` in `dal.py`.
    for s in [
        "preparing",
        "translating",
        "reviewing",
        "translating_subtitles",
        "parsing",
        "pausing",
        "curating",
    ] {
        assert!(ACTIVE_STATUSES.contains(&s), "{s} missing from ACTIVE_STATUSES");
    }
}

#[test]
fn audience_book_rejects_empty_and_missing_fmt() {
    assert!(!Audience::Book.visible_for(None));
    assert!(!Audience::Book.visible_for(Some("")));
    assert!(!Audience::Book.visible_for(Some("srt")));
    assert!(Audience::Book.visible_for(Some("epub")));
}

#[test]
fn audience_all_and_srt_rules() {
    assert!(Audience::All.visible_for(None));
    assert!(Audience::All.visible_for(Some("")));
    assert!(Audience::Srt.visible_for(Some("srt")));
    assert!(!Audience::Srt.visible_for(Some("epub")));
    assert!(!Audience::Srt.visible_for(None));
}

#[test]
fn invalidate_project_is_hermetic_no_op_off_fresh_client() {
    // No cache entries, no network: must simply not panic.
    let client = Client::new("http://127.0.0.1:9".into(), "t".into());
    client.invalidate_project("proj-1");
    client.invalidate_project("");
}

#[test]
fn project_create_serializes_expected_body() {
    let body = crate::client::ProjectCreate {
        name: "Demo".into(),
        source_lang: "en".into(),
        target_lang: "zh".into(),
        strategy: serde_json::json!({}),
        prepare: true,
        pdf_backend: None,
        translation_mode: "translate".into(),
    };
    let v = serde_json::to_value(&body).unwrap();
    assert_eq!(v["name"], "Demo");
    assert_eq!(v["prepare"], true);
    assert!(v.get("pdf_backend").is_none());
}
