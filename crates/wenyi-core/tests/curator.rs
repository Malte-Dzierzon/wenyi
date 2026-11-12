//! Curator parity: index-verified scans, excerpt picks, pass statistics.

use std::path::PathBuf;
use wenyi_core::curator::{pick_excerpts, Curator, TermOccurrence, CURATOR_BATCH_SIZE};
use wenyi_core::glossary::{self, GlossaryTerm};
use wenyi_core::llm::compat::Message;
use wenyi_core::llm::fake::{FakeClient, FakeHandler};
use wenyi_core::model::{BookId, ChapterId, ProjectId, SegmentId};
use wenyi_core::store;

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wenyi-core-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    store::db_path(&dir)
}

fn fixture(name: &str) -> (PathBuf, ProjectId, BookId) {
    let db = temp_db(name);
    let conn = store::open(&db).unwrap();
    let project = ProjectId::new("p").unwrap();
    let book = BookId::new("b").unwrap();
    store::create_project(&conn, &project, "t").unwrap();
    store::create_book(&conn, &book, &project, 0, "B").unwrap();
    drop(conn);
    (db, project, book)
}

fn seed_chapter(conn: &rusqlite::Connection, book: &BookId, ordinal: i64, texts: &[&str]) {
    let cid = ChapterId::new(format!("c{ordinal}")).unwrap();
    store::create_chapter(conn, &cid, book, ordinal, "").unwrap();
    let pairs: Vec<(SegmentId, String)> = texts
        .iter()
        .enumerate()
        .map(|(i, t)| {
            (
                SegmentId::new(format!("c{ordinal}-s{i}")).unwrap(),
                t.to_string(),
            )
        })
        .collect();
    store::insert_segments(conn, &cid, &pairs).unwrap();
}

fn occurrence(book: &BookId, segment: i64) -> TermOccurrence {
    TermOccurrence {
        book_id: book.clone(),
        chapter: 0,
        segment,
        source: format!("s{segment}"),
        target: None,
        matched: "k".to_string(),
    }
}

#[test]
fn excerpt_picks_match_oracle_first_middle_last() {
    let (_, _, book) = fixture("excerpt");
    let matches: Vec<TermOccurrence> = (0..10).map(|i| occurrence(&book, i)).collect();
    let picked = pick_excerpts(matches, 5);
    let segs: Vec<i64> = picked.iter().map(|o| o.segment).collect();
    // Oracle: [m0] + round(slot*step) + [m9], step = 9/4.
    assert_eq!(segs, vec![0, 2, 4, 7, 9]);
    let short: Vec<TermOccurrence> = (0..3).map(|i| occurrence(&book, i)).collect();
    assert_eq!(pick_excerpts(short, 5).len(), 3);
}

#[test]
fn index_scan_finds_terms_and_aliases_with_boundaries() {
    let (db, project, book) = fixture("scan");
    let conn = store::open(&db).unwrap();
    seed_chapter(
        &conn,
        &book,
        0,
        &[
            "The old Wolf prowls.",
            "Wolfish behavior.",
            "綾小路 arrives.",
        ],
    );
    let mut wolf = GlossaryTerm::new("Wolf", "狼");
    wolf.aliases = vec!["綾小路".to_string()];
    glossary::upsert_term(&conn, &project, "en", "zh", &wolf, Some(0)).unwrap();
    let terms = glossary::all_terms(&conn, &project, "en", "zh").unwrap();
    let hits = Curator::scan_book(&conn, &book, &terms).unwrap();
    let matched: Vec<(i64, String)> = hits
        .iter()
        .map(|(_, _, seg, m, _)| (*seg, m.clone()))
        .collect();
    // "Wolf" matches segment 0 only ("Wolfish" fails the boundary gate);
    // the CJK alias matches segment 2 as substring.
    assert!(matched.iter().any(|(s, _)| *s == 0), "{matched:?}");
    assert!(matched.iter().all(|(s, _)| *s != 1), "{matched:?}");
    assert!(matched.iter().any(|(s, _)| *s == 2), "{matched:?}");
    // Reindex persists rows; recount matches the scan.
    let count = Curator::reindex_book(&conn, &project, &book, "en", "zh").unwrap();
    assert_eq!(count, hits.len());
    assert_eq!(
        store::count_occurrences_for_term(&conn, &project, "en", "zh", "Wolf").unwrap(),
        hits.len() as i64
    );
}

#[test]
fn run_pass_verifies_and_files_conflicts_without_overwrite() {
    let (db, project, book) = fixture("pass");
    let conn = store::open(&db).unwrap();
    seed_chapter(&conn, &book, 0, &["Wolf prowls at night."]);
    glossary::upsert_term(
        &conn,
        &project,
        "en",
        "zh",
        &GlossaryTerm::new("Wolf", "狼"),
        Some(0),
    )
    .unwrap();
    glossary::upsert_term(
        &conn,
        &project,
        "en",
        "zh",
        &GlossaryTerm::new("Moon", "月"),
        Some(0),
    )
    .unwrap();
    Curator::reindex_book(&conn, &project, &book, "en", "zh").unwrap();
    let handler: FakeHandler = Box::new(|messages: &[Message], _tier: &str, _json: bool| {
        let prompt = messages.last().map(|m| m.content.as_str()).unwrap_or("");
        if prompt.contains("Wolf ->") {
            r#"{"status": "conflict", "proposed_target": "苍狼", "reason": "archaic register"}"#
                .to_string()
        } else {
            r#"{"status": "verified", "reason": "holds"}"#.to_string()
        }
    });
    let mut fake = FakeClient::new(Some(handler), None).unwrap();
    let curator = Curator::new("en", "zh");
    let terms = Curator::select_terms(&conn, &project, "en", "zh").unwrap();
    assert_eq!(terms.len(), 2);
    let mut events = Vec::new();
    let stats = curator
        .run_pass(
            &conn,
            &mut fake,
            &project,
            &terms,
            "B",
            &mut events,
            &|| false,
        )
        .unwrap();
    assert_eq!(stats["checked"], 2);
    assert_eq!(stats["verified"], 1);
    assert_eq!(stats["conflicts"], 1);
    assert_eq!(stats["skipped"], 0);
    // Established mapping untouched; conflict filed.
    let wolf = glossary::get_term(&conn, &project, "en", "zh", "Wolf")
        .unwrap()
        .unwrap();
    assert_eq!(wolf.target, "狼");
    assert_eq!(wolf.status, "conflict");
    assert_eq!(wolf.mentions, 1);
    let open = glossary::open_conflicts(&conn, &project, "en", "zh").unwrap();
    assert_eq!(open.len(), 1);
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|e| e.get("event").and_then(|v| v.as_str()))
        .collect();
    assert!(kinds.contains(&"curator_term_checked"));
    assert!(kinds.contains(&"curator_pass_finished"));
    // Record the pass: checked_at stamps + last_pass.
    Curator::record_pass(&conn, &project, "en", "zh", &terms, &stats).unwrap();
    let state = store::load_curator_state(&conn, &project, "en", "zh").unwrap();
    assert!(state
        .get("checked_at")
        .and_then(|v| v.as_object())
        .unwrap()
        .contains_key("Wolf"));
    assert_eq!(state["last_pass"]["checked"], 2);
}

#[test]
fn conflicts_sort_first_and_selection_caps_at_batch_size() {
    use rusqlite::params;
    let (db, project, _) = fixture("select");
    let conn = store::open(&db).unwrap();
    for i in 0..(CURATOR_BATCH_SIZE + 5) {
        let term = GlossaryTerm::new(format!("t{i:02}"), format!("x{i:02}"));
        glossary::upsert_term(&conn, &project, "en", "zh", &term, Some(0)).unwrap();
    }
    conn.execute(
        "INSERT INTO glossary_conflicts (project_id, source_lang, target_lang, source, existing_target, proposed_target, created_at) VALUES ('p', 'en', 'zh', 't20', 'x20', 'y20', 1)",
        params![],
    )
    .unwrap();
    let terms = Curator::select_terms(&conn, &project, "en", "zh").unwrap();
    assert_eq!(terms.len(), CURATOR_BATCH_SIZE);
    assert_eq!(terms[0].source, "t20");
}

#[test]
fn pass_stops_on_flag_and_skips_failed_terms() {
    let (db, project, book) = fixture("stop");
    let conn = store::open(&db).unwrap();
    seed_chapter(&conn, &book, 0, &["Wolf prowls."]);
    glossary::upsert_term(
        &conn,
        &project,
        "en",
        "zh",
        &GlossaryTerm::new("Wolf", "狼"),
        Some(0),
    )
    .unwrap();
    Curator::reindex_book(&conn, &project, &book, "en", "zh").unwrap();
    // Stop flag: pass breaks before checking.
    store::save_curator_state(&conn, &project, "en", "zh", r#"{"stop_requested": true}"#).unwrap();
    let handler: FakeHandler =
        Box::new(|_, _, _| r#"{"status": "verified", "reason": "x"}"#.to_string());
    let mut fake = FakeClient::new(Some(handler), None).unwrap();
    let curator = Curator::new("en", "zh");
    let terms = Curator::select_terms(&conn, &project, "en", "zh").unwrap();
    let mut events = Vec::new();
    let stats = curator
        .run_pass(
            &conn,
            &mut fake,
            &project,
            &terms,
            "B",
            &mut events,
            &|| false,
        )
        .unwrap();
    assert_eq!(stats["checked"], 0);
    assert_eq!(events[0]["event"], "curator_pass_stopped");
    // Failing model: term skipped, pass continues.
    store::save_curator_state(&conn, &project, "en", "zh", "{}").unwrap();
}
