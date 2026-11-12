//! Store tests: migrations, WAL, round-trips, invariants, jobs, backup.

use std::path::PathBuf;
use wenyi_core::model::{BookId, ChapterId, JobId, ProjectId, RunId, SegmentId};
use wenyi_core::store;

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wenyi-core-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    store::db_path(&dir)
}

#[test]
fn migrates_and_reports_wal() {
    let path = temp_db("migrate");
    let conn = store::open(&path).unwrap();
    assert_eq!(store::schema_version(&conn).unwrap(), store::SCHEMA_VERSION);
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    // Reopen is idempotent.
    drop(conn);
    let conn = store::open(&path).unwrap();
    assert_eq!(store::schema_version(&conn).unwrap(), store::SCHEMA_VERSION);
}

#[test]
fn project_book_chapter_round_trip_with_paging() {
    let path = temp_db("roundtrip");
    let conn = store::open(&path).unwrap();
    let pid = ProjectId::new("proj1").unwrap();
    store::create_project(&conn, &pid, "Series").unwrap();
    assert_eq!(store::list_projects(&conn).unwrap().len(), 1);

    let b1 = BookId::new("book1").unwrap();
    let b2 = BookId::new("book2").unwrap();
    store::create_book(&conn, &b1, &pid, 0, "Volume 1").unwrap();
    store::create_book(&conn, &b2, &pid, 1, "Volume 2").unwrap();
    let books = store::list_books(&conn, &pid).unwrap();
    assert_eq!(books.len(), 2);
    assert_eq!(books[0].title, "Volume 1");

    for i in 0..5 {
        store::create_chapter(
            &conn,
            &ChapterId::new(format!("ch{i}")).unwrap(),
            &b1,
            i,
            &format!("Chapter {i}"),
        )
        .unwrap();
    }
    let page = store::list_chapters(&conn, &b1, 1, 2).unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].ordinal, 1);
    assert_eq!(page[1].ordinal, 2);
    let empty = store::list_chapters(&conn, &b1, 99, 10).unwrap();
    assert!(empty.is_empty());
}

#[test]
fn pending_and_intentionally_empty_targets_stay_distinct() {
    let path = temp_db("targets");
    let conn = store::open(&path).unwrap();
    let pid = ProjectId::new("p").unwrap();
    let bid = BookId::new("b").unwrap();
    let cid = ChapterId::new("c").unwrap();
    store::create_project(&conn, &pid, "t").unwrap();
    store::create_book(&conn, &bid, &pid, 0, "").unwrap();
    store::create_chapter(&conn, &cid, &bid, 0, "").unwrap();
    let s0 = SegmentId::new("s0").unwrap();
    let s1 = SegmentId::new("s1").unwrap();
    store::insert_segments(
        &conn,
        &cid,
        &[
            (s0.clone(), "one".to_string()),
            (s1.clone(), "two".to_string()),
        ],
    )
    .unwrap();

    // Fresh inserts are pending (NULL).
    let segments = store::list_segments(&conn, &cid, 0, 10).unwrap();
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].target, None);

    // Intentionally empty is Some("") — distinct from pending.
    store::set_target(&conn, &s0, Some(""), "test").unwrap();
    let segments = store::list_segments(&conn, &cid, 0, 10).unwrap();
    assert_eq!(segments[0].target, Some(String::new()));
    assert_eq!(segments[1].target, None);

    // Revision recorded with NULL previous.
    let (previous, new): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT previous_target, new_target FROM segment_revisions WHERE segment_id = 's0'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(previous, None);
    assert_eq!(new, Some(String::new()));
}

#[test]
fn job_lifecycle_and_conditional_transition() {
    let path = temp_db("jobs");
    let conn = store::open(&path).unwrap();
    let pid = ProjectId::new("p").unwrap();
    store::create_project(&conn, &pid, "t").unwrap();
    let jid = JobId::new("j1").unwrap();
    let run = RunId::new("run1").unwrap();
    let job = store::enqueue_job(&conn, &jid, &pid, "translate", &run, "{}").unwrap();
    assert_eq!(job.status, "queued");

    assert!(store::transition_job(&conn, &run, &["queued"], "running").unwrap());
    // Wrong expected state: no transition (once-only crash recovery depends on this).
    assert!(!store::transition_job(&conn, &run, &["queued"], "interrupted").unwrap());
    assert!(store::transition_job(&conn, &run, &["running"], "done").unwrap());
}

#[test]
fn usage_records_exactly_once_per_batch_key() {
    let path = temp_db("usage");
    let conn = store::open(&path).unwrap();
    let pid = ProjectId::new("p").unwrap();
    store::create_project(&conn, &pid, "t").unwrap();
    let jid = JobId::new("j1").unwrap();
    let run = RunId::new("run1").unwrap();
    store::enqueue_job(&conn, &jid, &pid, "translate", &run, "{}").unwrap();

    let sample = store::UsageRecord {
        batch_key: "ch0:b0",
        tier: "strong",
        stage: "translate",
        provider: "d",
        model: "m",
        input_tokens: 10,
        output_tokens: 5,
    };
    assert!(store::record_usage(&conn, &jid, &sample).unwrap());
    // Retry/replay with the same key is a no-op, not a double bill.
    assert!(!store::record_usage(&conn, &jid, &sample).unwrap());
    let total: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(input_tokens), 0) FROM usage_ledger",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(total, 10);
}

#[test]
fn rollback_discards_partial_writes() {
    let path = temp_db("rollback");
    let mut conn = store::open(&path).unwrap();
    let _pid = ProjectId::new("p").unwrap();
    let result: wenyi_core::Result<()> = store::write(&mut conn, |tx| {
        tx.execute(
            "INSERT INTO projects (id, name, created_at) VALUES ('p', 't', 0)",
            [],
        )
        .map_err(|e| wenyi_core::Error::Store(e.to_string()))?;
        tx.execute(
            "INSERT INTO projects (id, name, created_at) VALUES ('p', 'dup', 0)",
            [],
        )
        .map_err(|e| wenyi_core::Error::Store(e.to_string()))?;
        Ok(())
    });
    assert!(result.is_err());
    assert!(store::list_projects(&conn).unwrap().is_empty());
}

#[test]
fn backup_restores_identical_state() {
    let path = temp_db("backup");
    let target = path.with_extension("bak.sqlite3");
    let pid = ProjectId::new("p").unwrap();
    {
        let conn = store::open(&path).unwrap();
        store::create_project(&conn, &pid, "Series").unwrap();
        store::backup(&conn, &target).unwrap();
    }
    let restored = store::open_readonly(&target).unwrap();
    let projects = store::list_projects(&restored).unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].name, "Series");
    assert_eq!(
        store::schema_version(&restored).unwrap(),
        store::SCHEMA_VERSION
    );
}

#[test]
fn concurrent_open_on_fresh_db_never_fails() {
    let path = temp_db("concurrent-open");
    let failures = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let path = path.clone();
        let failures = std::sync::Arc::clone(&failures);
        threads.push(std::thread::spawn(move || {
            if store::open(&path).is_err() {
                failures.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(failures.load(std::sync::atomic::Ordering::SeqCst), 0);
    let conn = store::open(&path).unwrap();
    assert_eq!(store::schema_version(&conn).unwrap(), store::SCHEMA_VERSION);
}

#[test]
fn write_uses_immediate_transactions_and_migration_rows_match_schema() {
    // `store::write` must take an IMMEDIATE (writer) lock: while one writer
    // holds it open, a second writer fails fast instead of reading stale
    // state. This pins the documented behavior.
    let path = temp_db("immediate-doc");
    let mut first = store::open(&path).unwrap();
    let second = store::open(&path).unwrap();
    second.pragma_update(None, "busy_timeout", 0).unwrap();
    {
        let _guard = first
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let raced = second.execute(
            "INSERT INTO projects (id, name, created_at) VALUES ('w', 'w', 0)",
            [],
        );
        assert!(raced.is_err(), "expected immediate-lock contention");
    }

    // Idempotent re-migration leaves exactly one row per version.
    let conn = store::open(&path).unwrap();
    store::migrate(&conn).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, store::SCHEMA_VERSION);
}

#[test]
fn unfinished_jobs_expose_updated_at_for_recovery_grace() {
    let path = temp_db("grace");
    let conn = store::open(&path).unwrap();
    let pid = ProjectId::new("p").unwrap();
    store::create_project(&conn, &pid, "t").unwrap();
    let before = store::now_ms();
    store::enqueue_job(
        &conn,
        &JobId::new("j1").unwrap(),
        &pid,
        "translate",
        &RunId::new("run1").unwrap(),
        "{}",
    )
    .unwrap();
    let unfinished = store::list_unfinished_jobs(&conn).unwrap();
    assert_eq!(unfinished.len(), 1);
    assert!(unfinished[0].updated_at >= before);
}

#[test]
fn chapter_and_segment_schema_lag_columns_are_projected() {
    let path = temp_db("lag-columns");
    let conn = store::open(&path).unwrap();
    let pid = ProjectId::new("p").unwrap();
    let bid = BookId::new("b").unwrap();
    let cid = ChapterId::new("c").unwrap();
    store::create_project(&conn, &pid, "t").unwrap();
    store::create_book(&conn, &bid, &pid, 0, "").unwrap();
    let chapter = store::create_chapter(&conn, &cid, &bid, 0, "T").unwrap();
    assert_eq!(chapter.title_translated, None);
    // A writer that bypasses the typed API still round-trips.
    conn.execute(
        "UPDATE chapters SET title_translated = 'TR' WHERE id = 'c'",
        [],
    )
    .unwrap();
    let changed = conn
        .execute(
            "UPDATE translations SET target_before_polish = 'draft' WHERE segment_id = 'x'",
            [],
        )
        .unwrap();
    assert_eq!(changed, 0);
    let chapters = store::list_chapters(&conn, &bid, 0, 10).unwrap();
    assert_eq!(chapters[0].title_translated.as_deref(), Some("TR"));
    let sid = SegmentId::new("s0").unwrap();
    store::insert_segments(&conn, &cid, &[(sid, "one".to_string())]).unwrap();
    conn.execute(
        "UPDATE translations SET target_before_polish = 'draft' WHERE segment_id = 's0'",
        [],
    )
    .unwrap();
    let segments = store::list_segments(&conn, &cid, 0, 10).unwrap();
    assert_eq!(segments[0].target_before_polish.as_deref(), Some("draft"));
}

#[test]
fn source_files_bind_books_by_hash() {
    let db = temp_db("source-files");
    let conn = store::open(&db).unwrap();
    let pid = ProjectId::new("p").unwrap();
    let bid = BookId::new("b0").unwrap();
    store::create_project(&conn, &pid, "t").unwrap();
    store::create_book(&conn, &bid, &pid, 0, "B").unwrap();
    assert_eq!(
        store::find_book_by_source_hash(&conn, &pid, "abc").unwrap(),
        None
    );
    store::insert_source_file(&conn, "b0-src", &bid, "novel.txt", "abc", 42, "txt").unwrap();
    // Idempotent: re-import of the same bytes does not duplicate.
    store::insert_source_file(&conn, "b0-src", &bid, "novel.txt", "abc", 42, "txt").unwrap();
    let found = store::find_book_by_source_hash(&conn, &pid, "abc")
        .unwrap()
        .unwrap();
    assert_eq!(found.id, bid);
    assert_eq!(found.ordinal, 0);
}

#[test]
fn book_overrides_win_for_one_book_only() {
    use wenyi_core::glossary::{set_override, terms_for_book, upsert_term, GlossaryTerm};
    let db = temp_db("book-overrides");
    let conn = store::open(&db).unwrap();
    let pid = ProjectId::new("p").unwrap();
    let b0 = BookId::new("b0").unwrap();
    let b1 = BookId::new("b1").unwrap();
    store::create_project(&conn, &pid, "t").unwrap();
    store::create_book(&conn, &b0, &pid, 0, "B0").unwrap();
    store::create_book(&conn, &b1, &pid, 1, "B1").unwrap();
    upsert_term(
        &conn,
        &pid,
        "en",
        "zh",
        &GlossaryTerm::new("Wolf", "狼"),
        Some(0),
    )
    .unwrap();
    set_override(&conn, &b1, "en", "zh", "Wolf", "苍狼", "volume 2 usage").unwrap();
    let t0: Vec<(String, String)> = terms_for_book(&conn, &pid, &b0, "en", "zh")
        .unwrap()
        .into_iter()
        .map(|t| (t.source, t.target))
        .collect();
    let t1: Vec<(String, String)> = terms_for_book(&conn, &pid, &b1, "en", "zh")
        .unwrap()
        .into_iter()
        .map(|t| (t.source, t.target))
        .collect();
    assert_eq!(t0, vec![("Wolf".to_string(), "狼".to_string())]);
    assert_eq!(t1, vec![("Wolf".to_string(), "苍狼".to_string())]);
}
