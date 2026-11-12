//! Series style memory: versioned profiles, overrides, proposals.

use std::path::PathBuf;
use wenyi_core::model::{BookId, ProjectId};
use wenyi_core::{store, style};

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wenyi-core-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    store::db_path(&dir)
}

fn fixture(name: &str) -> (PathBuf, ProjectId, BookId, BookId) {
    let db = temp_db(name);
    let conn = store::open(&db).unwrap();
    let project = ProjectId::new("p").unwrap();
    let b0 = BookId::new("b0").unwrap();
    let b1 = BookId::new("b1").unwrap();
    store::create_project(&conn, &project, "t").unwrap();
    store::create_book(&conn, &b0, &project, 0, "B0").unwrap();
    store::create_book(&conn, &b1, &project, 1, "B1").unwrap();
    drop(conn);
    (db, project, b0, b1)
}

fn analysis(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
        .collect()
}

#[test]
fn seed_publishes_v1_then_only_fills_missing_keys() {
    let (db, project, b0, b1) = fixture("seed");
    let conn = store::open(&db).unwrap();
    let v1 = style::seed_from_analysis(
        &conn,
        &project,
        &b0,
        "en",
        "zh",
        &analysis(&[("genre", "Epic"), ("tone", "Solemn")]),
        "fp1",
    )
    .unwrap();
    assert_eq!(v1.version, 1);
    // Second book with a conflicting tone and one new key: tone kept, new
    // key filled into v2.
    let v2 = style::seed_from_analysis(
        &conn,
        &project,
        &b1,
        "en",
        "zh",
        &analysis(&[("tone", "Playful"), ("pacing", "Brisk")]),
        "fp1",
    )
    .unwrap();
    assert_eq!(v2.version, 2);
    let live: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&v2.profile_json).unwrap();
    assert_eq!(live["tone"], "Solemn");
    assert_eq!(live["pacing"], "Brisk");
    // Third book with no new keys: no new version.
    let same = style::seed_from_analysis(
        &conn,
        &project,
        &b1,
        "en",
        "zh",
        &analysis(&[("genre", "Other")]),
        "fp1",
    )
    .unwrap();
    assert_eq!(same.version, 2);
}

#[test]
fn book_overrides_merge_and_pin_to_version() {
    let (db, project, b0, b1) = fixture("override");
    let conn = store::open(&db).unwrap();
    style::seed_from_analysis(
        &conn,
        &project,
        &b0,
        "en",
        "zh",
        &analysis(&[("tone", "Solemn")]),
        "fp1",
    )
    .unwrap();
    store::save_book_style_override(&conn, &b1, "en", "zh", 1, r#"{"tone":"Playful"}"#).unwrap();
    let s0 = style::snapshot_for_book(&conn, &project, &b0, "en", "zh").unwrap();
    let s1 = style::snapshot_for_book(&conn, &project, &b1, "en", "zh").unwrap();
    assert_eq!(s0.profile_version, 1);
    assert!(s0.brief.contains("Solemn"), "{}", s0.brief);
    assert!(s1.brief.contains("Playful"), "{}", s1.brief);
    assert!(!s0.brief.contains("Playful"), "{}", s0.brief);
    // A new profile version orphans the pin: overrides ignored until re-pin.
    store::publish_style_profile(
        &conn,
        &project,
        "en",
        "zh",
        r#"{"tone":"Solemn"}"#,
        "[]",
        "fp2",
    )
    .unwrap();
    let s1_stale = style::snapshot_for_book(&conn, &project, &b1, "en", "zh").unwrap();
    assert_eq!(s1_stale.profile_version, 2);
    assert!(s1_stale.brief.contains("Solemn"), "{}", s1_stale.brief);
}

#[test]
fn proposal_accept_merges_and_rejects_stale_before() {
    let (db, project, b0, _) = fixture("proposal");
    let conn = store::open(&db).unwrap();
    style::seed_from_analysis(
        &conn,
        &project,
        &b0,
        "en",
        "zh",
        &analysis(&[("tone", "Solemn")]),
        "fp1",
    )
    .unwrap();
    let id = store::save_style_proposal(
        &conn,
        &project,
        "en",
        "zh",
        Some(&b0),
        "tone",
        r#""Solemn""#,
        r#""Playful""#,
        "chapter 9 shifts voice",
        0.8,
        "[]",
        "fp1",
    )
    .unwrap();
    let published = style::accept_proposal(&conn, &project, "en", "zh", id)
        .unwrap()
        .unwrap();
    assert_eq!(published.version, 2);
    let live: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&published.profile_json).unwrap();
    assert_eq!(live["tone"], "Playful");
    assert!(store::pending_style_proposals(&conn, &project, "en", "zh")
        .unwrap()
        .is_empty());
    // Re-accept of the same (now non-pending) id: no-op.
    assert!(style::accept_proposal(&conn, &project, "en", "zh", id)
        .unwrap()
        .is_none());
    // Stale before-value: rejected.
    let stale = store::save_style_proposal(
        &conn,
        &project,
        "en",
        "zh",
        None,
        "tone",
        r#""Solemn""#,
        r#""Dry""#,
        "old read",
        0.5,
        "[]",
        "fp1",
    )
    .unwrap();
    assert!(style::accept_proposal(&conn, &project, "en", "zh", stale)
        .unwrap()
        .is_none());
    assert_eq!(
        store::pending_style_proposals(&conn, &project, "en", "zh")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn empty_project_snapshots_version_zero() {
    let (db, project, b0, _) = fixture("empty");
    let conn = store::open(&db).unwrap();
    let snap = style::snapshot_for_book(&conn, &project, &b0, "en", "zh").unwrap();
    assert_eq!(snap.profile_version, 0);
    assert!(snap.brief.is_empty());
}

#[test]
fn observe_chapter_stores_proposals_and_leaves_profile_live() {
    use wenyi_core::config;
    use wenyi_core::llm::compat::Message;
    use wenyi_core::llm::fake::{FakeClient, FakeHandler};
    use wenyi_core::model::ChapterId;
    use wenyi_core::pipeline::Pipeline;

    let dir = std::env::temp_dir().join(format!("wenyi-core-test-observe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = store::db_path(&dir);
    let conn = store::open(&db).unwrap();
    let project = ProjectId::new("p").unwrap();
    let book = BookId::new("b").unwrap();
    store::create_project(&conn, &project, "t").unwrap();
    store::create_book(&conn, &book, &project, 0, "B").unwrap();
    let chapter = ChapterId::new("c0").unwrap();
    store::create_chapter(&conn, &chapter, &book, 0, "C").unwrap();
    let sid = wenyi_core::model::SegmentId::new("s0").unwrap();
    store::insert_segments(&conn, &chapter, &[(sid.clone(), "Source one.".to_string())]).unwrap();
    store::set_target(&conn, &sid, Some("译文一。"), "translate").unwrap();
    style::seed_from_analysis(
        &conn,
        &project,
        &book,
        "en",
        "zh",
        &analysis(&[("tone", "Solemn")]),
        "fp1",
    )
    .unwrap();
    let handler: FakeHandler = Box::new(|_messages: &[Message], _tier: &str, _json: bool| {
        r#"{"proposals": [{"property": "tone", "after": "Playful", "reason": "banter", "confidence": 0.9, "scope": "series"}]}"#.to_string()
    });
    let cfg: config::Config = config::load(
        "language: {source: en, target: zh}\nllm:\n  preset: fake\n  models:\n    default_strong: {provider: default, model: p}\n    default_cheap: {provider: default, model: f}\n",
    )
    .unwrap();
    let mut fake = FakeClient::new(Some(handler), None).unwrap();
    let mut pipeline = Pipeline::new(cfg, &mut fake);
    let mut events = Vec::new();
    pipeline
        .observe_chapter(&conn, &project, &book, 0, &mut events)
        .unwrap();
    // Profile untouched; one pending proposal; event emitted.
    let live = store::latest_style_profile(&conn, &project, "en", "zh")
        .unwrap()
        .unwrap();
    assert_eq!(live.version, 1);
    assert!(live.profile_json.contains("Solemn"));
    let pending = store::pending_style_proposals(&conn, &project, "en", "zh").unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].property, "tone");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "style_observed");
    assert_eq!(events[0]["proposals"], 1);
}
