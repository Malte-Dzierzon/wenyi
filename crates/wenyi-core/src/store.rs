//! Workspace SQLite store: schema, migrations, transactions.
//!
//! Single `workspace.sqlite3` file (WAL), short write transactions only. Never
//! hold a transaction across model, file-conversion, or network I/O. Schema
//! evolves through numbered migrations tracked in `schema_migrations`.

use rusqlite::{params, Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{BookId, ChapterId, JobId, ProjectId, RunId, SegmentId};
use crate::{Error, Result};

/// Current schema version. Bump with each additive migration.
pub const SCHEMA_VERSION: i64 = 2;

/// Filename of the workspace database.
pub const DB_FILENAME: &str = "workspace.sqlite3";

/// Migration 1: foundation tables (projects/books/files/chapters/segments/
/// translations/jobs/events/usage/artifacts). Later slices add tables.
const MIGRATION_1: &str = r#"
CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    settings_json TEXT NOT NULL DEFAULT '{}'
);
CREATE TABLE books (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    UNIQUE(project_id, ordinal)
);
CREATE TABLE source_files (
    id TEXT PRIMARY KEY,
    book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    filename TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    bytes INTEGER NOT NULL DEFAULT 0,
    fmt TEXT NOT NULL DEFAULT '',
    meta_json TEXT NOT NULL DEFAULT '{}',
    stored_path TEXT NOT NULL DEFAULT '',
    UNIQUE(book_id, sha256)
);
CREATE TABLE chapters (
    id TEXT PRIMARY KEY,
    book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    href TEXT NOT NULL DEFAULT '',
    toc_entry_id TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending',
    meta_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(book_id, ordinal)
);
CREATE INDEX chapters_book ON chapters(book_id, ordinal);
CREATE TABLE segments (
    id TEXT PRIMARY KEY,
    chapter_id TEXT NOT NULL REFERENCES chapters(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    kind TEXT NOT NULL DEFAULT 'text',
    source TEXT NOT NULL,
    anchor TEXT NOT NULL DEFAULT '',
    resource_href TEXT NOT NULL DEFAULT '',
    cont INTEGER NOT NULL DEFAULT 0,
    meta_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(chapter_id, ordinal)
);
CREATE INDEX segments_chapter ON segments(chapter_id, ordinal);
CREATE TABLE translations (
    segment_id TEXT PRIMARY KEY REFERENCES segments(id) ON DELETE CASCADE,
    -- NULL = pending, '' = intentionally empty (oracle parity, never merge).
    target TEXT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    version INTEGER NOT NULL DEFAULT 1,
    style_profile_version INTEGER NULL,
    glossary_version INTEGER NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX translations_status ON translations(status);
CREATE TABLE segment_revisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    segment_id TEXT NOT NULL REFERENCES segments(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    previous_target TEXT NULL,
    new_target TEXT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX revisions_segment ON segment_revisions(segment_id, id);
CREATE TABLE jobs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued',
    run_id TEXT NOT NULL UNIQUE,
    params_json TEXT NOT NULL DEFAULT '{}',
    result_json TEXT NULL,
    error TEXT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX jobs_project_status ON jobs(project_id, status, updated_at);
CREATE TABLE job_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    ts INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX job_events_job ON job_events(job_id, ts);
CREATE TABLE usage_ledger (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    batch_key TEXT NOT NULL UNIQUE,
    tier TEXT NOT NULL DEFAULT '',
    stage TEXT NOT NULL DEFAULT '',
    provider TEXT NOT NULL DEFAULT '',
    model TEXT NOT NULL DEFAULT '',
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cost REAL NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE TABLE artifacts (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value_json TEXT NOT NULL DEFAULT '{}',
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, key)
);
CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at INTEGER NOT NULL
);
"#;
/// Migration 2 (Slice 4): glossary terms + conflicts, chapter digest/title
/// columns, polish baselines, book analysis. Additive only.
const MIGRATION_2: &str = r#"
CREATE TABLE glossary_terms (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    source_lang TEXT NOT NULL DEFAULT '',
    target_lang TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL,
    target TEXT NOT NULL,
    reading TEXT NOT NULL DEFAULT '',
    type TEXT NOT NULL DEFAULT 'term',
    gender TEXT NOT NULL DEFAULT '',
    aliases_json TEXT NOT NULL DEFAULT '[]',
    first_chapter INTEGER NULL,
    note TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'ok',
    mentions INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    UNIQUE(project_id, source_lang, target_lang, source)
);
CREATE INDEX glossary_terms_project ON glossary_terms(project_id, source_lang, target_lang, id);
CREATE TABLE glossary_conflicts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    source_lang TEXT NOT NULL DEFAULT '',
    target_lang TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL,
    existing_target TEXT NOT NULL DEFAULT '',
    proposed_target TEXT NOT NULL DEFAULT '',
    chapter INTEGER NULL,
    note TEXT NOT NULL DEFAULT '',
    resolved INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE INDEX glossary_conflicts_open ON glossary_conflicts(project_id, source_lang, target_lang, resolved, created_at);
ALTER TABLE chapters ADD COLUMN title_translated TEXT NULL;
ALTER TABLE translations ADD COLUMN target_before_polish TEXT NULL;
CREATE TABLE book_analysis (
    book_id TEXT PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
    analysis_json TEXT NOT NULL DEFAULT '{}',
    book_synopsis TEXT NOT NULL DEFAULT '',
    book_synopsis_meta_json TEXT NOT NULL DEFAULT '{}',
    updated_at INTEGER NOT NULL
);
CREATE TABLE chapter_digests (
    chapter_id TEXT PRIMARY KEY REFERENCES chapters(id) ON DELETE CASCADE,
    digest TEXT NOT NULL DEFAULT '',
    complete INTEGER NOT NULL DEFAULT 0,
    policy TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL
);
CREATE TABLE translation_context (
    book_id TEXT PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
    context_json TEXT NOT NULL DEFAULT '{}',
    updated_at INTEGER NOT NULL
);
CREATE TABLE batch_checkpoints (
    chapter_id TEXT NOT NULL REFERENCES chapters(id) ON DELETE CASCADE,
    glossary_key TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (chapter_id, glossary_key)
);
"#;

/// Epoch milliseconds for timestamps.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn map_err(e: rusqlite::Error) -> Error {
    Error::Store(e.to_string())
}

/// Open (creating parent dirs) and migrate the workspace database.
///
/// Concurrent openers serialize inside `migrate` (`BEGIN IMMEDIATE` with
/// the version gate under the lock), but `BEGIN IMMEDIATE` itself fails
/// fast with `SQLITE_BUSY` when a sibling holds RESERVED — the busy
/// timeout does not cover that path. Opening retries lock errors with a
/// short bounded backoff so parallel opens on a fresh DB converge.
pub fn open(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                path: parent.to_path_buf(),
                message: e.to_string(),
            })?;
        }
    }
    let mut last = None;
    for attempt in 0..10u64 {
        match open_once(db_path) {
            Ok(conn) => return Ok(conn),
            Err(Error::Store(message)) if is_lock_message(&message) => {
                last = Some(message);
                std::thread::sleep(std::time::Duration::from_millis(10 * (attempt + 1)));
            }
            Err(other) => return Err(other),
        }
    }
    Err(Error::Store(
        last.unwrap_or_else(|| "database is locked".to_string()),
    ))
}

/// True for transient SQLite lock contention worth retrying.
fn is_lock_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("locked") || lower.contains("busy")
}
/// Single open attempt: connect, set pragmas, migrate.
fn open_once(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .map_err(map_err)?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))
        .map_err(map_err)?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(map_err)?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(map_err)?;
    migrate(&conn)?;
    Ok(conn)
}
/// Open read-only (fails if the database does not exist).
pub fn open_readonly(db_path: &Path) -> Result<Connection> {
    let conn =
        Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(map_err)?;
    conn.pragma_update(None, "query_only", "ON")
        .map_err(map_err)?;
    Ok(conn)
}

/// Apply pending migrations inside one `BEGIN IMMEDIATE ... COMMIT` block so
/// concurrent openers serialize on the writer lock (with the 5 s busy
/// timeout) instead of racing DDL against each other.
///
/// Idempotence comes from the `schema_migrations` version gate read *inside*
/// the lock: a loser re-reads the gate after the winner commits and skips
/// already-applied bodies. A failed body rolls back and leaves no version
/// row, so a retry re-attempts the same version.
pub fn migrate(conn: &Connection) -> Result<()> {
    immediate_batch(conn, || {
        let applied: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if applied < 1 {
            conn.execute_batch(MIGRATION_1).map_err(map_err)?;
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (1, ?)",
                params![now_ms()],
            )
            .map_err(map_err)?;
        }
        if applied < 2 {
            conn.execute_batch(MIGRATION_2).map_err(map_err)?;
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (2, ?)",
                params![now_ms()],
            )
            .map_err(map_err)?;
        }
        Ok(())
    })
}

/// Current applied schema version (0 for a fresh pre-migration handle).
pub fn schema_version(conn: &Connection) -> Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )
    .map_err(map_err)
}

/// Run `f` inside an immediate write transaction (short by contract).
pub fn write<T>(
    conn: &mut Connection,
    f: impl FnOnce(&rusqlite::Transaction) -> Result<T>,
) -> Result<T> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(map_err)?;
    let out = f(&tx)?;
    tx.commit().map_err(map_err)?;
    Ok(out)
}

/// Back up the live database to `dest` (same-directory temp + atomic rename
/// by the caller; this only copies bytes via the SQLite backup API).
pub fn backup(conn: &Connection, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                path: parent.to_path_buf(),
                message: e.to_string(),
            })?;
        }
    }
    let mut target = Connection::open(dest).map_err(map_err)?;
    let backup = rusqlite::backup::Backup::new(conn, &mut target).map_err(map_err)?;
    backup
        .run_to_completion(128, std::time::Duration::ZERO, None)
        .map_err(map_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Typed row operations (Slice 1: projects/books/chapters/segments/jobs)
// ---------------------------------------------------------------------------

/// Project row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// Identity.
    pub id: ProjectId,
    /// Display name.
    pub name: String,
    /// Creation time (epoch ms).
    pub created_at: i64,
}

/// Book row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Book {
    /// Identity.
    pub id: BookId,
    /// Owning project.
    pub project_id: ProjectId,
    /// Position within the project.
    pub ordinal: i64,
    /// Display title.
    pub title: String,
}

/// Chapter row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    /// Identity.
    pub id: ChapterId,
    /// Owning book.
    pub book_id: BookId,
    /// Position within the book.
    pub ordinal: i64,
    /// Display title.
    pub title: String,
    /// Workflow status.
    pub status: String,
    /// Translated title (`None` = untranslated; MIGRATION_2 column).
    pub title_translated: Option<String>,
}

/// Segment row (immutable source + optional translation target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Identity.
    pub id: SegmentId,
    /// Owning chapter.
    pub chapter_id: ChapterId,
    /// Position within the chapter.
    pub ordinal: i64,
    /// Segment kind (text/heading/...).
    pub kind: String,
    /// Immutable source text.
    pub source: String,
    /// Translation target: `None` = pending, `Some("")` = intentionally empty.
    pub target: Option<String>,
    /// Pre-polish target baseline (`None` = no baseline recorded).
    pub target_before_polish: Option<String>,
    /// Stable markup anchor (empty when none).
    pub anchor: String,
}

/// Job row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Identity.
    pub id: JobId,
    /// Owning project.
    pub project_id: ProjectId,
    /// Job kind (translate, review, export, ...).
    pub kind: String,
    /// Lifecycle status.
    pub status: String,
    /// Durable run identity.
    pub run_id: RunId,
    /// Last write time (epoch ms; periodic recovery grace).
    pub updated_at: i64,
}

/// Create a project. Fails if the id already exists.
pub fn create_project(conn: &Connection, id: &ProjectId, name: &str) -> Result<Project> {
    let created_at = now_ms();
    conn.execute(
        "INSERT INTO projects (id, name, created_at) VALUES (?, ?, ?)",
        params![id.as_str(), name, created_at],
    )
    .map_err(map_err)?;
    Ok(Project {
        id: id.clone(),
        name: name.to_string(),
        created_at,
    })
}

/// Get one project by id.
pub fn get_project(conn: &Connection, id: &ProjectId) -> Result<Option<Project>> {
    let mut stmt = conn
        .prepare("SELECT id, name, created_at FROM projects WHERE id = ?")
        .map_err(map_err)?;
    let mut rows = stmt.query(params![id.as_str()]).map_err(map_err)?;
    let Some(row) = rows.next().map_err(map_err)? else {
        return Ok(None);
    };
    Ok(Some(Project {
        id: ProjectId::new(row.get::<_, String>(0).map_err(map_err)?)?,
        name: row.get(1).map_err(map_err)?,
        created_at: row.get(2).map_err(map_err)?,
    }))
}

/// List all projects ordered by creation time.
pub fn list_projects(conn: &Connection) -> Result<Vec<Project>> {
    let mut stmt = conn
        .prepare("SELECT id, name, created_at FROM projects ORDER BY created_at, id")
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, created_at) = row.map_err(map_err)?;
        out.push(Project {
            id: ProjectId::new(id)?,
            name,
            created_at,
        });
    }
    Ok(out)
}

/// Create a book within a project.
pub fn create_book(
    conn: &Connection,
    id: &BookId,
    project_id: &ProjectId,
    ordinal: i64,
    title: &str,
) -> Result<Book> {
    conn.execute(
        "INSERT INTO books (id, project_id, ordinal, title, created_at) VALUES (?, ?, ?, ?, ?)",
        params![id.as_str(), project_id.as_str(), ordinal, title, now_ms()],
    )
    .map_err(map_err)?;
    Ok(Book {
        id: id.clone(),
        project_id: project_id.clone(),
        ordinal,
        title: title.to_string(),
    })
}

/// List a project's books in ordinal order.
pub fn list_books(conn: &Connection, project_id: &ProjectId) -> Result<Vec<Book>> {
    let mut stmt = conn
        .prepare("SELECT id, project_id, ordinal, title FROM books WHERE project_id = ? ORDER BY ordinal")
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![project_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, project, ordinal, title) = row.map_err(map_err)?;
        out.push(Book {
            id: BookId::new(id)?,
            project_id: ProjectId::new(project)?,
            ordinal,
            title,
        });
    }
    Ok(out)
}

/// Create a chapter within a book.
pub fn create_chapter(
    conn: &Connection,
    id: &ChapterId,
    book_id: &BookId,
    ordinal: i64,
    title: &str,
) -> Result<Chapter> {
    conn.execute(
        "INSERT INTO chapters (id, book_id, ordinal, title) VALUES (?, ?, ?, ?)",
        params![id.as_str(), book_id.as_str(), ordinal, title],
    )
    .map_err(map_err)?;
    Ok(Chapter {
        id: id.clone(),
        book_id: book_id.clone(),
        ordinal,
        title: title.to_string(),
        status: "pending".to_string(),
        title_translated: None,
    })
}

/// Page through a book's chapters in ordinal order.
pub fn list_chapters(
    conn: &Connection,
    book_id: &BookId,
    offset: i64,
    limit: i64,
) -> Result<Vec<Chapter>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, book_id, ordinal, title, status, title_translated FROM chapters \
             WHERE book_id = ? ORDER BY ordinal LIMIT ? OFFSET ?",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![book_id.as_str(), limit, offset], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, book, ordinal, title, status, title_translated) = row.map_err(map_err)?;
        out.push(Chapter {
            id: ChapterId::new(id)?,
            book_id: BookId::new(book)?,
            ordinal,
            title,
            status,
            title_translated,
        });
    }
    Ok(out)
}

/// Insert segments with pending translations in one atomic batch.
///
/// Both rows per segment (plus the batch) land in a single `BEGIN IMMEDIATE
/// ... COMMIT` block, so a mid-batch failure rolls everything back and
/// concurrent writers serialize on the lock.
pub fn insert_segments(
    conn: &Connection,
    chapter_id: &ChapterId,
    segments: &[(SegmentId, String)],
) -> Result<()> {
    immediate_batch(conn, || {
        let mut insert_segment = conn
            .prepare("INSERT INTO segments (id, chapter_id, ordinal, source) VALUES (?, ?, ?, ?)")
            .map_err(map_err)?;
        let mut insert_translation = conn
            .prepare(
                "INSERT INTO translations (segment_id, target, updated_at) VALUES (?, NULL, ?)",
            )
            .map_err(map_err)?;
        let now = now_ms();
        for (ordinal, (id, source)) in segments.iter().enumerate() {
            insert_segment
                .execute(params![
                    id.as_str(),
                    chapter_id.as_str(),
                    ordinal as i64,
                    source
                ])
                .map_err(map_err)?;
            insert_translation
                .execute(params![id.as_str(), now])
                .map_err(map_err)?;
        }
        Ok(())
    })
}
/// Patch one segment's format columns (kind/anchor/resource/cont/meta).
#[allow(clippy::too_many_arguments)] // One call per staged segment; columns are fixed.
pub fn update_segment_format(
    conn: &Connection,
    chapter_id: &ChapterId,
    ordinal: i64,
    kind: &str,
    anchor: &str,
    resource_href: &str,
    cont: bool,
    meta_json: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE segments SET kind = ?, anchor = ?, resource_href = ?, cont = ?, meta_json = ? \
         WHERE chapter_id = ? AND ordinal = ?",
        params![
            kind,
            anchor,
            resource_href,
            i64::from(cont),
            meta_json,
            chapter_id.as_str(),
            ordinal
        ],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Run a closure inside `BEGIN IMMEDIATE ... COMMIT`, rolling back on error.
fn immediate_batch(conn: &Connection, body: impl FnOnce() -> Result<()>) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE").map_err(map_err)?;
    match body() {
        Ok(()) => conn.execute_batch("COMMIT").map_err(map_err)?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    Ok(())
}

/// Page through a chapter's segments with their targets in ordinal order.
pub fn list_segments(
    conn: &Connection,
    chapter_id: &ChapterId,
    offset: i64,
    limit: i64,
) -> Result<Vec<Segment>> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.chapter_id, s.ordinal, s.kind, s.source, t.target, \
             t.target_before_polish, s.anchor \
             FROM segments s LEFT JOIN translations t ON t.segment_id = s.id \
             WHERE s.chapter_id = ? ORDER BY s.ordinal LIMIT ? OFFSET ?",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![chapter_id.as_str(), limit, offset], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
            ))
        })
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, chapter, ordinal, kind, source, target, target_before_polish, anchor) =
            row.map_err(map_err)?;
        out.push(Segment {
            id: SegmentId::new(id)?,
            chapter_id: ChapterId::new(chapter)?,
            ordinal,
            kind,
            source,
            target,
            target_before_polish,
            anchor,
        });
    }
    Ok(out)
}

/// Set a segment's translation target (`None` clears back to pending).
/// Records a revision row and bumps the translation version atomically.
pub fn set_target(
    conn: &Connection,
    segment_id: &SegmentId,
    target: Option<&str>,
    kind: &str,
) -> Result<()> {
    let previous: Option<String> = conn
        .query_row(
            "SELECT target FROM translations WHERE segment_id = ?",
            params![segment_id.as_str()],
            |row| row.get(0),
        )
        .map_err(map_err)?;
    conn.execute(
        "INSERT INTO segment_revisions (segment_id, kind, previous_target, new_target, created_at) \
         VALUES (?, ?, ?, ?, ?)",
        params![segment_id.as_str(), kind, previous, target, now_ms()],
    )
    .map_err(map_err)?;
    conn.execute(
        "UPDATE translations SET target = ?, version = version + 1, updated_at = ? \
         WHERE segment_id = ?",
        params![target, now_ms(), segment_id.as_str()],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Enqueue a job with `queued` status.
pub fn enqueue_job(
    conn: &Connection,
    id: &JobId,
    project_id: &ProjectId,
    kind: &str,
    run_id: &RunId,
    params_json: &str,
) -> Result<Job> {
    let now = now_ms();
    conn.execute(
        "INSERT INTO jobs (id, project_id, kind, status, run_id, params_json, created_at, updated_at) \
         VALUES (?, ?, ?, 'queued', ?, ?, ?, ?)",
        params![id.as_str(), project_id.as_str(), kind, run_id.as_str(), params_json, now, now],
    )
    .map_err(map_err)?;
    Ok(Job {
        id: id.clone(),
        project_id: project_id.clone(),
        kind: kind.to_string(),
        status: "queued".to_string(),
        run_id: run_id.clone(),
        updated_at: now,
    })
}

/// Transition a job's status only from one of `expected` (conditional write).
/// Returns `true` when the transition applied.
pub fn transition_job(
    conn: &Connection,
    run_id: &RunId,
    expected: &[&str],
    status: &str,
) -> Result<bool> {
    let placeholders = expected.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("UPDATE jobs SET status = ?, updated_at = ? WHERE run_id = ? AND status IN ({placeholders})");
    let mut stmt = conn.prepare(&sql).map_err(map_err)?;
    let now = now_ms();
    let run = run_id.as_str();
    let mut values: Vec<&dyn rusqlite::ToSql> = vec![
        &status as &dyn rusqlite::ToSql,
        &now as &dyn rusqlite::ToSql,
    ];
    values.push(&run as &dyn rusqlite::ToSql);
    for status in expected {
        values.push(status as &dyn rusqlite::ToSql);
    }
    let changed = stmt.execute(values.as_slice()).map_err(map_err)?;
    Ok(changed == 1)
}
/// Get a job by durable run identity.
pub fn get_job_by_run(conn: &Connection, run_id: &RunId) -> Result<Option<Job>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, project_id, kind, status, run_id, updated_at FROM jobs WHERE run_id = ?",
        )
        .map_err(map_err)?;
    let mut rows = stmt.query(params![run_id.as_str()]).map_err(map_err)?;
    let Some(row) = rows.next().map_err(map_err)? else {
        return Ok(None);
    };
    Ok(Some(Job {
        id: JobId::new(row.get::<_, String>(0).map_err(map_err)?)?,
        project_id: ProjectId::new(row.get::<_, String>(1).map_err(map_err)?)?,
        kind: row.get(2).map_err(map_err)?,
        status: row.get(3).map_err(map_err)?,
        run_id: RunId::new(row.get::<_, String>(4).map_err(map_err)?)?,
        updated_at: row.get(5).map_err(map_err)?,
    }))
}

/// List active (`queued`/`running`) jobs for admission checks.
pub fn list_active_jobs(conn: &Connection, project_id: &ProjectId) -> Result<Vec<Job>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, project_id, kind, status, run_id, updated_at FROM jobs \
             WHERE project_id = ? AND status IN ('queued', 'running') \
             ORDER BY updated_at DESC",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![project_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(map_err)?;
    let mut jobs = Vec::new();
    for row in rows {
        let (id, project_id, kind, status, run_id, updated_at) = row.map_err(map_err)?;
        jobs.push(Job {
            id: JobId::new(id)?,
            project_id: ProjectId::new(project_id)?,
            kind,
            status,
            run_id: RunId::new(run_id)?,
            updated_at,
        });
    }
    Ok(jobs)
}

/// Active (`queued`/`running`) jobs with their admission `params_json`.
///
/// Companion to [`list_active_jobs`] for lane checks: selecting the params in
/// the same query avoids an N+1 re-read per row. `Job` itself stays lean.
pub fn list_active_jobs_with_params(
    conn: &Connection,
    project_id: &ProjectId,
) -> Result<Vec<(Job, String)>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, project_id, kind, status, run_id, updated_at, params_json FROM jobs \
             WHERE project_id = ? AND status IN ('queued', 'running') \
             ORDER BY updated_at DESC",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![project_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(map_err)?;
    let mut jobs = Vec::new();
    for row in rows {
        let (id, project_id, kind, status, run_id, updated_at, params_json) =
            row.map_err(map_err)?;
        jobs.push((
            Job {
                id: JobId::new(id)?,
                project_id: ProjectId::new(project_id)?,
                kind,
                status,
                run_id: RunId::new(run_id)?,
                updated_at,
            },
            params_json,
        ));
    }
    Ok(jobs)
}

/// All non-terminal jobs across projects (startup/periodic recovery scan).
pub fn list_unfinished_jobs(conn: &Connection) -> Result<Vec<Job>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, project_id, kind, status, run_id, updated_at FROM jobs \
             WHERE status IN ('queued', 'running') ORDER BY updated_at",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(map_err)?;
    let mut jobs = Vec::new();
    for row in rows {
        let (id, project_id, kind, status, run_id, updated_at) = row.map_err(map_err)?;
        jobs.push(Job {
            id: JobId::new(id)?,
            project_id: ProjectId::new(project_id)?,
            kind,
            status,
            run_id: RunId::new(run_id)?,
            updated_at,
        });
    }
    Ok(jobs)
}

/// Set a job's terminal detail (result/error payload + status transition).
pub fn finish_job(
    conn: &Connection,
    run_id: &RunId,
    status: &str,
    error: Option<&str>,
) -> Result<bool> {
    let changed = conn
        .execute(
            "UPDATE jobs SET status = ?, error = ?, updated_at = ? \
             WHERE run_id = ? AND status IN ('queued', 'running')",
            params![status, error, now_ms(), run_id.as_str()],
        )
        .map_err(map_err)?;
    Ok(changed == 1)
}
/// Set a chapter's workflow status.
pub fn set_chapter_status(conn: &Connection, chapter_id: &ChapterId, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE chapters SET status = ? WHERE id = ?",
        params![status, chapter_id.as_str()],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Ordinals of chapters not yet done, in order (oracle `pending_chapters`).
pub fn pending_chapters(conn: &Connection, book_id: &BookId) -> Result<Vec<i64>> {
    let mut stmt = conn
        .prepare(
            "SELECT ordinal FROM chapters WHERE book_id = ? AND status != 'done' ORDER BY ordinal",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![book_id.as_str()], |row| row.get::<_, i64>(0))
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(map_err)?);
    }
    Ok(out)
}

/// Set a chapter's translated title.
pub fn set_title_translated(
    conn: &Connection,
    chapter_id: &ChapterId,
    title: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE chapters SET title_translated = ? WHERE id = ?",
        params![title, chapter_id.as_str()],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Set a segment's pre-polish baseline (preserved for comparisons).
pub fn set_target_before_polish(
    conn: &Connection,
    segment_id: &SegmentId,
    baseline: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE translations SET target_before_polish = ?, updated_at = ? WHERE segment_id = ?",
        params![baseline, now_ms(), segment_id.as_str()],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Save a segment's target plus its pre-polish baseline atomically.
pub fn set_target_with_baseline(
    conn: &Connection,
    segment_id: &SegmentId,
    target: Option<&str>,
    baseline: Option<&str>,
    kind: &str,
) -> Result<()> {
    set_target(conn, segment_id, target, kind)?;
    set_target_before_polish(conn, segment_id, baseline)?;
    Ok(())
}

/// Save the book analysis document (upsert).
pub fn save_analysis(conn: &Connection, book_id: &BookId, analysis_json: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO book_analysis (book_id, analysis_json, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(book_id) DO UPDATE SET analysis_json = excluded.analysis_json, \
         updated_at = excluded.updated_at",
        params![book_id.as_str(), analysis_json, now_ms()],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Load the book analysis document, if any.
pub fn load_analysis(conn: &Connection, book_id: &BookId) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT analysis_json FROM book_analysis WHERE book_id = ?")
        .map_err(map_err)?;
    let mut rows = stmt.query(params![book_id.as_str()]).map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(None),
        Some(row) => Ok(Some(row.get::<_, String>(0).map_err(map_err)?)),
    }
}

/// Save the whole-book synopsis plus its inputs fingerprint (upsert).
pub fn save_synopsis(
    conn: &Connection,
    book_id: &BookId,
    synopsis: &str,
    meta_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO book_analysis (book_id, book_synopsis, book_synopsis_meta_json, updated_at) \
         VALUES (?, ?, ?, ?) ON CONFLICT(book_id) DO UPDATE SET book_synopsis = excluded.book_synopsis, \
         book_synopsis_meta_json = excluded.book_synopsis_meta_json, updated_at = excluded.updated_at",
        params![book_id.as_str(), synopsis, meta_json, now_ms()],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Load the whole-book synopsis plus its inputs fingerprint, if any.
pub fn load_synopsis(conn: &Connection, book_id: &BookId) -> Result<Option<(String, String)>> {
    let mut stmt = conn
        .prepare(
            "SELECT book_synopsis, book_synopsis_meta_json FROM book_analysis WHERE book_id = ?",
        )
        .map_err(map_err)?;
    let mut rows = stmt.query(params![book_id.as_str()]).map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(None),
        Some(row) => Ok(Some((
            row.get::<_, String>(0).map_err(map_err)?,
            row.get::<_, String>(1).map_err(map_err)?,
        ))),
    }
}

/// Save one chapter digest with completion marker + policy (upsert).
pub fn save_digest(
    conn: &Connection,
    chapter_id: &ChapterId,
    digest: &str,
    complete: bool,
    policy: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO chapter_digests (chapter_id, digest, complete, policy, updated_at) \
         VALUES (?, ?, ?, ?, ?) ON CONFLICT(chapter_id) DO UPDATE SET digest = excluded.digest, \
         complete = excluded.complete, policy = excluded.policy, updated_at = excluded.updated_at",
        params![
            chapter_id.as_str(),
            digest,
            i64::from(complete),
            policy,
            now_ms()
        ],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Load one chapter digest, if any.
pub fn load_digest(
    conn: &Connection,
    chapter_id: &ChapterId,
) -> Result<Option<(String, bool, String)>> {
    let mut stmt = conn
        .prepare("SELECT digest, complete, policy FROM chapter_digests WHERE chapter_id = ?")
        .map_err(map_err)?;
    let mut rows = stmt.query(params![chapter_id.as_str()]).map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(None),
        Some(row) => Ok(Some((
            row.get::<_, String>(0).map_err(map_err)?,
            row.get::<_, i64>(1).map_err(map_err)? != 0,
            row.get::<_, String>(2).map_err(map_err)?,
        ))),
    }
}

/// Save the rolling translation context (upsert).
pub fn save_context(conn: &Connection, book_id: &BookId, context_json: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO translation_context (book_id, context_json, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(book_id) DO UPDATE SET context_json = excluded.context_json, \
         updated_at = excluded.updated_at",
        params![book_id.as_str(), context_json, now_ms()],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Load the rolling translation context, if any.
pub fn load_context(conn: &Connection, book_id: &BookId) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT context_json FROM translation_context WHERE book_id = ?")
        .map_err(map_err)?;
    let mut rows = stmt.query(params![book_id.as_str()]).map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(None),
        Some(row) => Ok(Some(row.get::<_, String>(0).map_err(map_err)?)),
    }
}

/// Glossary checkpoint key for one batch (oracle `batch_glossary_key`).
pub fn batch_glossary_key(start_index: usize, count: usize) -> String {
    format!("{start_index}:{count}")
}

/// Completed batch glossary-checkpoint keys for one chapter.
pub fn completed_batch_glossary_keys(
    conn: &Connection,
    chapter_id: &ChapterId,
) -> Result<std::collections::BTreeSet<String>> {
    let mut stmt = conn
        .prepare("SELECT glossary_key FROM batch_checkpoints WHERE chapter_id = ?")
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![chapter_id.as_str()], |row| row.get::<_, String>(0))
        .map_err(map_err)?;
    let mut out = std::collections::BTreeSet::new();
    for row in rows {
        out.insert(row.map_err(map_err)?);
    }
    Ok(out)
}

/// Mark one batch's glossary extraction complete (idempotent).
pub fn complete_batch_glossary_key(
    conn: &Connection,
    chapter_id: &ChapterId,
    key: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO batch_checkpoints (chapter_id, glossary_key, created_at) VALUES (?, ?, ?) \
         ON CONFLICT(chapter_id, glossary_key) DO NOTHING",
        params![chapter_id.as_str(), key, now_ms()],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Append a durable job event.
pub fn append_job_event(
    conn: &Connection,
    job_id: &JobId,
    kind: &str,
    payload_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO job_events (job_id, ts, kind, payload_json) VALUES (?, ?, ?, ?)",
        params![job_id.as_str(), now_ms(), kind, payload_json],
    )
    .map_err(map_err)?;
    Ok(())
}

/// One usage observation bound to a batch identity.
#[derive(Debug, Clone)]
pub struct UsageRecord<'a> {
    /// Idempotency key (job + batch identity).
    pub batch_key: &'a str,
    /// Routing tier.
    pub tier: &'a str,
    /// Pipeline stage / operation.
    pub stage: &'a str,
    /// Provider name.
    pub provider: &'a str,
    /// Model name.
    pub model: &'a str,
    /// Input tokens consumed.
    pub input_tokens: i64,
    /// Output tokens consumed.
    pub output_tokens: i64,
}

/// Append a usage entry exactly once per `batch_key` (idempotent; returns
/// `false` when the key was already recorded, e.g. on retry/replay).
pub fn record_usage(conn: &Connection, job_id: &JobId, sample: &UsageRecord) -> Result<bool> {
    let changed = conn
        .execute(
            "INSERT INTO usage_ledger \
             (job_id, batch_key, tier, stage, provider, model, input_tokens, output_tokens, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(batch_key) DO NOTHING",
            params![
                job_id.as_str(),
                sample.batch_key,
                sample.tier,
                sample.stage,
                sample.provider,
                sample.model,
                sample.input_tokens,
                sample.output_tokens,
                now_ms()
            ],
        )
        .map_err(map_err)?;
    Ok(changed == 1)
}

/// Workspace path helper: database file inside `dir`.
pub fn db_path(dir: &Path) -> PathBuf {
    dir.join(DB_FILENAME)
}
