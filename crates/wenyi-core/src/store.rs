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
pub const SCHEMA_VERSION: i64 = 6;

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
/// Migration 3 (Slice 5): series style memory, glossary book overrides,
/// curator state, occurrence index. Additive only.
const MIGRATION_3: &str = r#"
CREATE TABLE style_profiles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    source_lang TEXT NOT NULL DEFAULT '',
    target_lang TEXT NOT NULL DEFAULT '',
    version INTEGER NOT NULL DEFAULT 1,
    profile_json TEXT NOT NULL DEFAULT '{}',
    evidence_json TEXT NOT NULL DEFAULT '[]',
    prompt_version TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    UNIQUE(project_id, source_lang, target_lang, version)
);
CREATE INDEX style_profiles_project ON style_profiles(project_id, source_lang, target_lang, version);
CREATE TABLE book_style_overrides (
    book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    source_lang TEXT NOT NULL DEFAULT '',
    target_lang TEXT NOT NULL DEFAULT '',
    profile_version INTEGER NOT NULL DEFAULT 1,
    overrides_json TEXT NOT NULL DEFAULT '{}',
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (book_id, source_lang, target_lang)
);
CREATE TABLE style_proposals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    source_lang TEXT NOT NULL DEFAULT '',
    target_lang TEXT NOT NULL DEFAULT '',
    book_id TEXT NULL REFERENCES books(id) ON DELETE CASCADE,
    property TEXT NOT NULL DEFAULT '',
    before_json TEXT NOT NULL DEFAULT 'null',
    after_json TEXT NOT NULL DEFAULT 'null',
    reason TEXT NOT NULL DEFAULT '',
    confidence REAL NOT NULL DEFAULT 0,
    evidence_json TEXT NOT NULL DEFAULT '[]',
    prompt_version TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending',
    created_at INTEGER NOT NULL
);
CREATE INDEX style_proposals_open ON style_proposals(project_id, source_lang, target_lang, status, created_at);
CREATE TABLE glossary_overrides (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    source_lang TEXT NOT NULL DEFAULT '',
    target_lang TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL,
    target TEXT NOT NULL,
    note TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL,
    UNIQUE(book_id, source_lang, target_lang, source)
);
CREATE INDEX glossary_overrides_book ON glossary_overrides(book_id, source_lang, target_lang, source);
CREATE TABLE curator_state (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    source_lang TEXT NOT NULL DEFAULT '',
    target_lang TEXT NOT NULL DEFAULT '',
    state_json TEXT NOT NULL DEFAULT '{}',
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, source_lang, target_lang)
);
CREATE TABLE term_occurrences (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    source_lang TEXT NOT NULL DEFAULT '',
    target_lang TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL,
    book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    chapter_id TEXT NOT NULL REFERENCES chapters(id) ON DELETE CASCADE,
    segment_id TEXT NOT NULL REFERENCES segments(id) ON DELETE CASCADE,
    match_kind TEXT NOT NULL DEFAULT 'source',
    matched TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    UNIQUE(project_id, source_lang, target_lang, source, segment_id, match_kind)
);
CREATE INDEX term_occurrences_term ON term_occurrences(project_id, source_lang, target_lang, source, segment_id);
CREATE INDEX term_occurrences_segment ON term_occurrences(segment_id);
"#;
/// Migration 4 (Slice 6): review runs, chunks, traces, checkpoints.
/// Replaces the oracle `reviews/review-{stamp}/` artifact tree with typed
/// tables; round-scoped paths become (review_id, round, key) rows.
const MIGRATION_4: &str = r#"
CREATE TABLE review_runs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'running',
    termination TEXT NOT NULL DEFAULT 'not_started',
    reviewed_content_digest TEXT NOT NULL DEFAULT '',
    metadata_json TEXT NOT NULL DEFAULT '{}',
    summary_json TEXT NOT NULL DEFAULT '{}',
    issues_json TEXT NOT NULL DEFAULT '[]',
    changes_json TEXT NOT NULL DEFAULT '[]',
    error_json TEXT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX review_runs_book ON review_runs(book_id, started_at);
CREATE INDEX review_runs_project ON review_runs(project_id, started_at);
CREATE TABLE review_chunks (
    review_id TEXT NOT NULL REFERENCES review_runs(id) ON DELETE CASCADE,
    chunk_id TEXT NOT NULL,
    result_json TEXT NOT NULL DEFAULT '{}',
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (review_id, chunk_id)
);
CREATE TABLE review_traces (
    review_id TEXT NOT NULL REFERENCES review_runs(id) ON DELETE CASCADE,
    round_number INTEGER NOT NULL DEFAULT 0,
    trace_key TEXT NOT NULL,
    trace_json TEXT NOT NULL DEFAULT '{}',
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (review_id, round_number, trace_key)
);
CREATE INDEX review_traces_round ON review_traces(review_id, round_number);
CREATE TABLE review_checkpoints (
    review_id TEXT PRIMARY KEY REFERENCES review_runs(id) ON DELETE CASCADE,
    checkpoint_json TEXT NOT NULL DEFAULT '{}',
    updated_at INTEGER NOT NULL
);
CREATE TABLE review_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    review_id TEXT NOT NULL REFERENCES review_runs(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL DEFAULT 0,
    ts INTEGER NOT NULL,
    kind TEXT NOT NULL DEFAULT '',
    payload_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX review_events_review ON review_events(review_id, seq);
"#;
/// Migration 5 (Slice 7+): durable export records for the in-process
/// `Service` (the old REST backend kept export state in memory).
/// Additive only: one table keyed by `(project_id)` with CASCADE delete.
const MIGRATION_5: &str = r#"
CREATE TABLE exports (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    format TEXT NOT NULL DEFAULT 'txt',
    status TEXT NOT NULL DEFAULT 'queued',
    path TEXT NULL,
    bytes INTEGER NULL,
    error TEXT NULL,
    options_json TEXT NOT NULL DEFAULT '{}',
    run_id TEXT NULL,
    created_at INTEGER NOT NULL,
    finished_at INTEGER NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX exports_project ON exports(project_id, updated_at);
"#;
/// Migration 6 (gap closure): cached word counts per chapter. Source words
/// are immutable after import; target words refresh on target writes.
const MIGRATION_6: &str = r#"
CREATE TABLE chapter_statistics (
    chapter_id TEXT PRIMARY KEY REFERENCES chapters(id) ON DELETE CASCADE,
    source_words INTEGER NOT NULL DEFAULT 0,
    target_words INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
);
"#;
/// Epoch milliseconds for timestamps.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Map a SQLite error into an engine error (service delete paths).
pub fn map_error(e: rusqlite::Error) -> Error {
    Error::Store(e.to_string())
}

pub(crate) fn map_err(e: rusqlite::Error) -> Error {
    map_error(e)
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
        if applied < 3 {
            conn.execute_batch(MIGRATION_3).map_err(map_err)?;
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (3, ?)",
                params![now_ms()],
            )
            .map_err(map_err)?;
        }
        if applied < 4 {
            conn.execute_batch(MIGRATION_4).map_err(map_err)?;
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (4, ?)",
                params![now_ms()],
            )
            .map_err(map_err)?;
        }
        if applied < 5 {
            conn.execute_batch(MIGRATION_5).map_err(map_err)?;
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (5, ?)",
                params![now_ms()],
            )
            .map_err(map_err)?;
        }
        if applied < 6 {
            conn.execute_batch(MIGRATION_6).map_err(map_err)?;
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (6, ?)",
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

/// Delete a project and its whole subtree (`ON DELETE CASCADE` on every
/// child table). Returns `true` when a row existed.
pub fn delete_project(conn: &Connection, id: &ProjectId) -> Result<bool> {
    let changed = conn
        .execute("DELETE FROM projects WHERE id = ?", params![id.as_str()])
        .map_err(map_err)?;
    Ok(changed == 1)
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

/// Source file row (immutable import identity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// Identity.
    pub id: String,
    /// Owning book.
    pub book_id: BookId,
    /// Original filename.
    pub filename: String,
    /// SHA-256 of the imported bytes.
    pub sha256: String,
    /// Byte size.
    pub bytes: i64,
    /// Detected format.
    pub fmt: String,
}

/// Record a source file import (idempotent per book + hash).
pub fn insert_source_file(
    conn: &Connection,
    id: &str,
    book_id: &BookId,
    filename: &str,
    sha256: &str,
    bytes: i64,
    fmt: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO source_files (id, book_id, filename, sha256, bytes, fmt) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(book_id, sha256) DO NOTHING",
        params![id, book_id.as_str(), filename, sha256, bytes, fmt],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Find the book holding a source hash within a project.
pub fn find_book_by_source_hash(
    conn: &Connection,
    project_id: &ProjectId,
    sha256: &str,
) -> Result<Option<Book>> {
    let row: Option<(String, String, i64, String)> = match conn.query_row(
        "SELECT b.id, b.project_id, b.ordinal, b.title \
         FROM books b JOIN source_files s ON s.book_id = b.id \
         WHERE b.project_id = ? AND s.sha256 = ? \
         ORDER BY b.ordinal LIMIT 1",
        params![project_id.as_str(), sha256],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ) {
        Ok(row) => Some(row),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(error) => return Err(map_err(error)),
    };
    row.map(|(id, project, ordinal, title)| {
        Ok(Book {
            id: BookId::new(id)?,
            project_id: ProjectId::new(project)?,
            ordinal,
            title,
        })
    })
    .transpose()
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

/// Slim segment scan row: `(ordinal, kind, source, target)`.
pub type SegmentScanRow = (i64, String, String, Option<String>);

/// Stream segments without materializing the full chapter (bounded memory).
/// Yields [`SegmentScanRow`] — enough for scans and exports.
pub fn iter_segments(conn: &Connection, chapter_id: &ChapterId) -> Result<Vec<SegmentScanRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT s.ordinal, s.kind, s.source, t.target              FROM segments s LEFT JOIN translations t ON t.segment_id = s.id              WHERE s.chapter_id = ? ORDER BY s.ordinal",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![chapter_id.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(map_err)?);
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
    set_target_stamped(conn, segment_id, target, kind, None)
}

/// `set_target` plus an optional style-profile version stamp.
pub fn set_target_stamped(
    conn: &Connection,
    segment_id: &SegmentId,
    target: Option<&str>,
    kind: &str,
    style_profile_version: Option<i64>,
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
        "UPDATE translations SET target = ?, version = version + 1, \
         style_profile_version = COALESCE(?, style_profile_version), updated_at = ? \
         WHERE segment_id = ?",
        params![target, style_profile_version, now_ms(), segment_id.as_str()],
    )
    .map_err(map_err)?;
    // Keep cached target counts fresh (single-chapter rescan).
    if let Ok(chapter_id) = conn.query_row(
        "SELECT chapter_id FROM segments WHERE id = ?",
        params![segment_id.as_str()],
        |row| row.get::<_, String>(0),
    ) {
        if let Ok(chapter_id) = ChapterId::new(chapter_id) {
            let _ = refresh_chapter_statistics(conn, &chapter_id);
        }
    }
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

/// `set_target_with_baseline` plus a style-profile version stamp.
pub fn set_target_stamped_with_baseline(
    conn: &Connection,
    segment_id: &SegmentId,
    target: Option<&str>,
    baseline: Option<&str>,
    kind: &str,
    style_profile_version: Option<i64>,
) -> Result<()> {
    set_target_stamped(conn, segment_id, target, kind, style_profile_version)?;
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

/// One published style profile version (immutable once written).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleProfile {
    /// Owning project.
    pub project_id: ProjectId,
    /// Source language.
    pub source_lang: String,
    /// Target language.
    pub target_lang: String,
    /// Monotonic version (1-based).
    pub version: i64,
    /// Profile payload (style fields, oracle analysis shape).
    pub profile_json: String,
    /// Evidence references.
    pub evidence_json: String,
    /// Prompt/model version that produced it.
    pub prompt_version: String,
}

/// Latest published style profile for a language pair, if any.
pub fn latest_style_profile(
    conn: &Connection,
    project_id: &ProjectId,
    source_lang: &str,
    target_lang: &str,
) -> Result<Option<StyleProfile>> {
    let mut stmt = conn
        .prepare(
            "SELECT version, profile_json, evidence_json, prompt_version FROM style_profiles \
             WHERE project_id = ? AND source_lang = ? AND target_lang = ? \
             ORDER BY version DESC LIMIT 1",
        )
        .map_err(map_err)?;
    let mut rows = stmt
        .query(params![project_id.as_str(), source_lang, target_lang])
        .map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(None),
        Some(row) => Ok(Some(StyleProfile {
            project_id: project_id.clone(),
            source_lang: source_lang.to_string(),
            target_lang: target_lang.to_string(),
            version: row.get(0).map_err(map_err)?,
            profile_json: row.get(1).map_err(map_err)?,
            evidence_json: row.get(2).map_err(map_err)?,
            prompt_version: row.get(3).map_err(map_err)?,
        })),
    }
}

/// Publish a new immutable profile version (next = latest + 1).
pub fn publish_style_profile(
    conn: &Connection,
    project_id: &ProjectId,
    source_lang: &str,
    target_lang: &str,
    profile_json: &str,
    evidence_json: &str,
    prompt_version: &str,
) -> Result<StyleProfile> {
    let version = latest_style_profile(conn, project_id, source_lang, target_lang)?
        .map(|profile| profile.version)
        .unwrap_or(0)
        + 1;
    conn.execute(
        "INSERT INTO style_profiles \
         (project_id, source_lang, target_lang, version, profile_json, evidence_json, \
         prompt_version, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            project_id.as_str(),
            source_lang,
            target_lang,
            version,
            profile_json,
            evidence_json,
            prompt_version,
            now_ms()
        ],
    )
    .map_err(map_err)?;
    Ok(StyleProfile {
        project_id: project_id.clone(),
        source_lang: source_lang.to_string(),
        target_lang: target_lang.to_string(),
        version,
        profile_json: profile_json.to_string(),
        evidence_json: evidence_json.to_string(),
        prompt_version: prompt_version.to_string(),
    })
}

/// Book-scoped style overrides pinned to one profile version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookStyleOverride {
    /// Owning book.
    pub book_id: BookId,
    /// Profile version the overrides apply to.
    pub profile_version: i64,
    /// Override payload (partial style fields).
    pub overrides_json: String,
}

/// Load a book's style overrides, if any.
pub fn load_book_style_override(
    conn: &Connection,
    book_id: &BookId,
    source_lang: &str,
    target_lang: &str,
) -> Result<Option<BookStyleOverride>> {
    let mut stmt = conn
        .prepare(
            "SELECT profile_version, overrides_json FROM book_style_overrides \
             WHERE book_id = ? AND source_lang = ? AND target_lang = ?",
        )
        .map_err(map_err)?;
    let mut rows = stmt
        .query(params![book_id.as_str(), source_lang, target_lang])
        .map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(None),
        Some(row) => Ok(Some(BookStyleOverride {
            book_id: book_id.clone(),
            profile_version: row.get(0).map_err(map_err)?,
            overrides_json: row.get(1).map_err(map_err)?,
        })),
    }
}

/// Save a book's overrides pinned to `profile_version` (upsert).
pub fn save_book_style_override(
    conn: &Connection,
    book_id: &BookId,
    source_lang: &str,
    target_lang: &str,
    profile_version: i64,
    overrides_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO book_style_overrides \
         (book_id, source_lang, target_lang, profile_version, overrides_json, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(book_id, source_lang, target_lang) DO UPDATE SET \
         profile_version = excluded.profile_version, \
         overrides_json = excluded.overrides_json, updated_at = excluded.updated_at",
        params![
            book_id.as_str(),
            source_lang,
            target_lang,
            profile_version,
            overrides_json,
            now_ms()
        ],
    )
    .map_err(map_err)?;
    Ok(())
}

/// One style proposal (pending until accepted/rejected/pinned).
#[derive(Debug, Clone)]
pub struct StyleProposal {
    /// Row id.
    pub id: i64,
    /// Affected property (top-level style field).
    pub property: String,
    /// Before value.
    pub before_json: String,
    /// After value.
    pub after_json: String,
    /// Rationale.
    pub reason: String,
    /// Confidence 0..=1.
    pub confidence: f64,
    /// Evidence references.
    pub evidence_json: String,
    /// Prompt/model version.
    pub prompt_version: String,
    /// Status (`pending`/`accepted`/`rejected`/`pinned`).
    pub status: String,
}

/// Save a proposal; returns the row id.
#[allow(clippy::too_many_arguments)] // One row, fixed columns.
pub fn save_style_proposal(
    conn: &Connection,
    project_id: &ProjectId,
    source_lang: &str,
    target_lang: &str,
    book_id: Option<&BookId>,
    property: &str,
    before_json: &str,
    after_json: &str,
    reason: &str,
    confidence: f64,
    evidence_json: &str,
    prompt_version: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO style_proposals \
         (project_id, source_lang, target_lang, book_id, property, before_json, after_json, \
         reason, confidence, evidence_json, prompt_version, status, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?)",
        params![
            project_id.as_str(),
            source_lang,
            target_lang,
            book_id.map(BookId::as_str),
            property,
            before_json,
            after_json,
            reason,
            confidence,
            evidence_json,
            prompt_version,
            now_ms()
        ],
    )
    .map_err(map_err)?;
    Ok(conn.last_insert_rowid())
}

/// List pending proposals in creation order.
pub fn pending_style_proposals(
    conn: &Connection,
    project_id: &ProjectId,
    source_lang: &str,
    target_lang: &str,
) -> Result<Vec<StyleProposal>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, property, before_json, after_json, reason, confidence, evidence_json, \
             prompt_version, status FROM style_proposals \
             WHERE project_id = ? AND source_lang = ? AND target_lang = ? AND status = 'pending' \
             ORDER BY id",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(
            params![project_id.as_str(), source_lang, target_lang],
            |row| {
                Ok(StyleProposal {
                    id: row.get(0)?,
                    property: row.get(1)?,
                    before_json: row.get(2)?,
                    after_json: row.get(3)?,
                    reason: row.get(4)?,
                    confidence: row.get(5)?,
                    evidence_json: row.get(6)?,
                    prompt_version: row.get(7)?,
                    status: row.get(8)?,
                })
            },
        )
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(map_err)?);
    }
    Ok(out)
}

/// Mark a proposal `accepted`/`rejected`/`pinned`; true when it flipped.
pub fn set_style_proposal_status(conn: &Connection, id: i64, status: &str) -> Result<bool> {
    let changed = conn
        .execute(
            "UPDATE style_proposals SET status = ? WHERE id = ? AND status = 'pending'",
            params![status, id],
        )
        .map_err(map_err)?;
    Ok(changed == 1)
}

/// One indexed term occurrence (curator scan cache).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermOccurrence {
    /// Matched term source.
    pub source: String,
    /// Owning book.
    pub book_id: BookId,
    /// Chapter identity.
    pub chapter_id: ChapterId,
    /// Segment identity.
    pub segment_id: SegmentId,
    /// `source` or `alias`.
    pub match_kind: String,
    /// Normalized key that matched.
    pub matched: String,
}

/// Replace one segment's occurrence rows (reindex after edits).
pub fn replace_segment_occurrences(
    conn: &Connection,
    project_id: &ProjectId,
    source_lang: &str,
    target_lang: &str,
    segment_id: &SegmentId,
    rows: &[(String, BookId, ChapterId, String, String)],
) -> Result<()> {
    conn.execute(
        "DELETE FROM term_occurrences WHERE segment_id = ?",
        params![segment_id.as_str()],
    )
    .map_err(map_err)?;
    for (source, book_id, chapter_id, match_kind, matched) in rows {
        conn.execute(
            "INSERT OR IGNORE INTO term_occurrences \
             (project_id, source_lang, target_lang, source, book_id, chapter_id, segment_id, \
             match_kind, matched, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                project_id.as_str(),
                source_lang,
                target_lang,
                source,
                book_id.as_str(),
                chapter_id.as_str(),
                segment_id.as_str(),
                match_kind,
                matched,
                now_ms()
            ],
        )
        .map_err(map_err)?;
    }
    Ok(())
}

/// Occurrence rows for one term, ordered by book/chapter/segment.
pub fn occurrences_for_term(
    conn: &Connection,
    project_id: &ProjectId,
    source_lang: &str,
    target_lang: &str,
    source: &str,
    limit: i64,
) -> Result<Vec<TermOccurrence>> {
    let mut stmt = conn
        .prepare(
            "SELECT o.source, o.book_id, o.chapter_id, o.segment_id, o.match_kind, o.matched \
             FROM term_occurrences o \
             JOIN books b ON b.id = o.book_id \
             JOIN chapters c ON c.id = o.chapter_id \
             JOIN segments s ON s.id = o.segment_id \
             WHERE o.project_id = ? AND o.source_lang = ? AND o.target_lang = ? AND o.source = ? \
             ORDER BY b.ordinal, c.ordinal, s.ordinal LIMIT ?",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(
            params![project_id.as_str(), source_lang, target_lang, source, limit],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (source, book, chapter, segment, match_kind, matched) = row.map_err(map_err)?;
        out.push(TermOccurrence {
            source,
            book_id: BookId::new(book)?,
            chapter_id: ChapterId::new(chapter)?,
            segment_id: SegmentId::new(segment)?,
            match_kind,
            matched,
        });
    }
    Ok(out)
}

/// Count indexed occurrences for one term.
pub fn count_occurrences_for_term(
    conn: &Connection,
    project_id: &ProjectId,
    source_lang: &str,
    target_lang: &str,
    source: &str,
) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM term_occurrences \
         WHERE project_id = ? AND source_lang = ? AND target_lang = ? AND source = ?",
        params![project_id.as_str(), source_lang, target_lang, source],
        |row| row.get(0),
    )
    .map_err(map_err)
}

/// Curator durable state (`checked_at` map + `stop_requested` flag).
pub fn load_curator_state(
    conn: &Connection,
    project_id: &ProjectId,
    source_lang: &str,
    target_lang: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut stmt = conn
        .prepare(
            "SELECT state_json FROM curator_state \
             WHERE project_id = ? AND source_lang = ? AND target_lang = ?",
        )
        .map_err(map_err)?;
    let mut rows = stmt
        .query(params![project_id.as_str(), source_lang, target_lang])
        .map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(serde_json::Map::new()),
        Some(row) => {
            let text: String = row.get(0).map_err(map_err)?;
            Ok(serde_json::from_str(&text).unwrap_or_default())
        }
    }
}

/// Save curator durable state (upsert).
pub fn save_curator_state(
    conn: &Connection,
    project_id: &ProjectId,
    source_lang: &str,
    target_lang: &str,
    state_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO curator_state (project_id, source_lang, target_lang, state_json, updated_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(project_id, source_lang, target_lang) DO UPDATE SET \
         state_json = excluded.state_json, updated_at = excluded.updated_at",
        params![
            project_id.as_str(),
            source_lang,
            target_lang,
            state_json,
            now_ms()
        ],
    )
    .map_err(map_err)?;
    Ok(())
}

/// One review run row (oracle `reviews/review-{id}/result.json` + metadata).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRun {
    /// Review identity (`review-{stamp}`).
    pub id: String,
    /// Owning project.
    pub project_id: ProjectId,
    /// Reviewed book.
    pub book_id: BookId,
    /// `running`/`completed`/`partial`/`interrupted`/`failed`.
    pub status: String,
    /// Termination reason.
    pub termination: String,
    /// Content digest of the formal text read.
    pub reviewed_content_digest: String,
    /// Config + guidance + glossary fingerprint.
    pub metadata_json: String,
    /// Result summary.
    pub summary_json: String,
    /// Final issues.
    pub issues_json: String,
    /// Shadow changes.
    pub changes_json: String,
}

/// Start a review run, or resume an existing resumable one (oracle
/// `ReviewRunStore.start`).
pub fn start_review_run(
    conn: &Connection,
    id: &str,
    project_id: &ProjectId,
    book_id: &BookId,
    reviewed_content_digest: &str,
    metadata_json: &str,
) -> Result<ReviewRun> {
    if let Some(existing) = load_review_run(conn, id)? {
        if ["running", "interrupted", "failed"].contains(&existing.status.as_str()) {
            conn.execute(
                "UPDATE review_runs SET status = 'running', termination = 'running', \
                 error_json = NULL, updated_at = ? WHERE id = ?",
                params![now_ms(), id],
            )
            .map_err(map_err)?;
            return load_review_run(conn, id)?.ok_or_else(|| Error::NotFound {
                kind: "review",
                id: id.to_string(),
            });
        }
        return Ok(existing);
    }
    let mut metadata: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(metadata_json).unwrap_or_default();
    metadata.insert(
        "reviewed_content_digest".to_string(),
        serde_json::Value::String(reviewed_content_digest.to_string()),
    );
    let now = now_ms();
    conn.execute(
        "INSERT INTO review_runs \
         (id, project_id, book_id, status, termination, reviewed_content_digest, \
         metadata_json, summary_json, issues_json, changes_json, started_at, updated_at) \
         VALUES (?, ?, ?, 'running', 'not_started', ?, ?, ?, '[]', '[]', ?, ?)",
        params![
            id,
            project_id.as_str(),
            book_id.as_str(),
            reviewed_content_digest,
            serde_json::Value::Object(metadata).to_string(),
            serde_json::json!({"issue_count": 0, "change_count": 0}).to_string(),
            now,
            now,
        ],
    )
    .map_err(map_err)?;
    load_review_run(conn, id)?.ok_or_else(|| Error::NotFound {
        kind: "review",
        id: id.to_string(),
    })
}

/// Load one review run.
pub fn load_review_run(conn: &Connection, id: &str) -> Result<Option<ReviewRun>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, project_id, book_id, status, termination, reviewed_content_digest, \
             metadata_json, summary_json, issues_json, changes_json \
             FROM review_runs WHERE id = ?",
        )
        .map_err(map_err)?;
    let mut rows = stmt.query(params![id]).map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(None),
        Some(row) => Ok(Some(ReviewRun {
            id: row.get(0).map_err(map_err)?,
            project_id: ProjectId::new(row.get::<_, String>(1).map_err(map_err)?)?,
            book_id: BookId::new(row.get::<_, String>(2).map_err(map_err)?)?,
            status: row.get(3).map_err(map_err)?,
            termination: row.get(4).map_err(map_err)?,
            reviewed_content_digest: row.get(5).map_err(map_err)?,
            metadata_json: row.get(6).map_err(map_err)?,
            summary_json: row.get(7).map_err(map_err)?,
            issues_json: row.get(8).map_err(map_err)?,
            changes_json: row.get(9).map_err(map_err)?,
        })),
    }
}

/// Finish a review run with its unified result (oracle `finish`).
#[allow(clippy::too_many_arguments)] // One row, fixed columns.
pub fn finish_review_run(
    conn: &Connection,
    id: &str,
    status: &str,
    termination: &str,
    summary_json: &str,
    issues_json: &str,
    changes_json: &str,
    error_json: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE review_runs SET status = ?, termination = ?, summary_json = ?, \
         issues_json = ?, changes_json = ?, error_json = ?, finished_at = ?, updated_at = ? \
         WHERE id = ?",
        params![
            status,
            termination,
            summary_json,
            issues_json,
            changes_json,
            error_json,
            now_ms(),
            now_ms(),
            id
        ],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Newest-first resumable run for a digest (oracle `find_resumable`).
pub fn find_resumable_review(
    conn: &Connection,
    book_id: &BookId,
    reviewed_content_digest: &str,
) -> Result<Option<ReviewRun>> {
    let mut stmt = conn
        .prepare(
            "SELECT id FROM review_runs \
             WHERE book_id = ? AND reviewed_content_digest = ? \
             AND status IN ('running', 'interrupted', 'failed') \
             ORDER BY started_at DESC LIMIT 1",
        )
        .map_err(map_err)?;
    let mut rows = stmt
        .query(params![book_id.as_str(), reviewed_content_digest])
        .map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(None),
        Some(row) => {
            let id: String = row.get(0).map_err(map_err)?;
            load_review_run(conn, &id)
        }
    }
}

/// Latest finished run for reports (oracle `load_latest_review_result`).
pub fn latest_finished_review(conn: &Connection, book_id: &BookId) -> Result<Option<ReviewRun>> {
    let mut stmt = conn
        .prepare(
            "SELECT id FROM review_runs \
             WHERE book_id = ? AND status IN ('completed', 'partial') \
             ORDER BY finished_at DESC LIMIT 1",
        )
        .map_err(map_err)?;
    let mut rows = stmt.query(params![book_id.as_str()]).map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(None),
        Some(row) => {
            let id: String = row.get(0).map_err(map_err)?;
            load_review_run(conn, &id)
        }
    }
}

/// Mark a chunk result (upsert).
pub fn save_review_chunk(
    conn: &Connection,
    review_id: &str,
    chunk_id: &str,
    result_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO review_chunks (review_id, chunk_id, result_json, updated_at) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT(review_id, chunk_id) DO UPDATE SET result_json = excluded.result_json, \
         updated_at = excluded.updated_at",
        params![review_id, chunk_id, result_json, now_ms()],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Load one chunk result.
pub fn load_review_chunk(
    conn: &Connection,
    review_id: &str,
    chunk_id: &str,
) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT result_json FROM review_chunks WHERE review_id = ? AND chunk_id = ?")
        .map_err(map_err)?;
    let mut rows = stmt.query(params![review_id, chunk_id]).map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(None),
        Some(row) => Ok(Some(row.get(0).map_err(map_err)?)),
    }
}

/// Save a round-scoped trace (upsert).
pub fn save_review_trace(
    conn: &Connection,
    review_id: &str,
    round_number: i64,
    trace_key: &str,
    trace_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO review_traces (review_id, round_number, trace_key, trace_json, updated_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(review_id, round_number, trace_key) DO UPDATE SET \
         trace_json = excluded.trace_json, updated_at = excluded.updated_at",
        params![review_id, round_number, trace_key, trace_json, now_ms()],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Load a round-scoped trace.
pub fn load_review_trace(
    conn: &Connection,
    review_id: &str,
    round_number: i64,
    trace_key: &str,
) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT trace_json FROM review_traces \
             WHERE review_id = ? AND round_number = ? AND trace_key = ?",
        )
        .map_err(map_err)?;
    let mut rows = stmt
        .query(params![review_id, round_number, trace_key])
        .map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(None),
        Some(row) => Ok(Some(row.get(0).map_err(map_err)?)),
    }
}

/// Save the session checkpoint (upsert).
pub fn save_review_checkpoint(
    conn: &Connection,
    review_id: &str,
    checkpoint_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO review_checkpoints (review_id, checkpoint_json, updated_at) \
         VALUES (?, ?, ?) \
         ON CONFLICT(review_id) DO UPDATE SET checkpoint_json = excluded.checkpoint_json, \
         updated_at = excluded.updated_at",
        params![review_id, checkpoint_json, now_ms()],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Load the session checkpoint.
pub fn load_review_checkpoint(conn: &Connection, review_id: &str) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT checkpoint_json FROM review_checkpoints WHERE review_id = ?")
        .map_err(map_err)?;
    let mut rows = stmt.query(params![review_id]).map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(None),
        Some(row) => Ok(Some(row.get(0).map_err(map_err)?)),
    }
}

/// Append a review event with a sequence number.
pub fn append_review_event(
    conn: &Connection,
    review_id: &str,
    kind: &str,
    payload_json: &str,
) -> Result<()> {
    let seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM review_events WHERE review_id = ?",
            params![review_id],
            |row| row.get(0),
        )
        .map_err(map_err)?;
    conn.execute(
        "INSERT INTO review_events (review_id, seq, ts, kind, payload_json) \
         VALUES (?, ?, ?, ?, ?)",
        params![review_id, seq, now_ms(), kind, payload_json],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Save a project artifact value (upsert).
pub fn save_artifact(
    conn: &Connection,
    project_id: &ProjectId,
    key: &str,
    value_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO artifacts (project_id, key, value_json, updated_at) VALUES (?, ?, ?, ?) \
         ON CONFLICT(project_id, key) DO UPDATE SET value_json = excluded.value_json, \
         updated_at = excluded.updated_at",
        params![project_id.as_str(), key, value_json, now_ms()],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Load a project artifact value.
pub fn load_artifact(
    conn: &Connection,
    project_id: &ProjectId,
    key: &str,
) -> Result<Option<String>> {
    let mut stmt = conn
        .prepare("SELECT value_json FROM artifacts WHERE project_id = ? AND key = ?")
        .map_err(map_err)?;
    let mut rows = stmt
        .query(params![project_id.as_str(), key])
        .map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        None => Ok(None),
        Some(row) => Ok(Some(row.get(0).map_err(map_err)?)),
    }
}

// ---------------------------------------------------------------------------
// Slice 7+: export records + segment history + stale-guarded edits
// ---------------------------------------------------------------------------

/// One durable export record (v1 replaces the old in-memory export state).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRecord {
    /// Identity.
    pub id: String,
    /// Owning project.
    pub project_id: ProjectId,
    /// Exported book.
    pub book_id: BookId,
    /// Export format (`txt`, `html`, `markdown`, `docx`, `epub`).
    pub format: String,
    /// Lifecycle status (`queued`/`running`/`done`/`error`).
    pub status: String,
    /// Output file path (set on success).
    pub path: Option<String>,
    /// Output file size in bytes (set on success).
    pub bytes: Option<i64>,
    /// Failure detail (set on error).
    pub error: Option<String>,
    /// Request options JSON.
    pub options_json: String,
    /// Driving job run identity (set once enqueued).
    pub run_id: Option<String>,
    /// Creation time (epoch ms).
    pub created_at: i64,
    /// Completion time (epoch ms, `None` while pending).
    pub finished_at: Option<i64>,
}

/// Row mapper for `exports` (12 columns; positional like the other row
/// constructors in this module).
#[allow(clippy::too_many_arguments)]
fn row_to_export(
    id: String,
    project_id: String,
    book_id: String,
    format: String,
    status: String,
    path: Option<String>,
    bytes: Option<i64>,
    error: Option<String>,
    options_json: String,
    run_id: Option<String>,
    created_at: i64,
    finished_at: Option<i64>,
) -> Result<ExportRecord> {
    Ok(ExportRecord {
        id,
        project_id: ProjectId::new(project_id)?,
        book_id: BookId::new(book_id)?,
        format,
        status,
        path,
        bytes,
        error,
        options_json,
        run_id,
        created_at,
        finished_at,
    })
}

/// Create an export record with `queued` status.
pub fn create_export(
    conn: &Connection,
    id: &str,
    project_id: &ProjectId,
    book_id: &BookId,
    format: &str,
    options_json: &str,
) -> Result<ExportRecord> {
    let created_at = now_ms();
    conn.execute(
        "INSERT INTO exports (id, project_id, book_id, format, status, options_json, \
         created_at, updated_at) VALUES (?, ?, ?, ?, 'queued', ?, ?, ?)",
        params![
            id,
            project_id.as_str(),
            book_id.as_str(),
            format,
            options_json,
            created_at,
            created_at
        ],
    )
    .map_err(map_err)?;
    row_to_export(
        id.to_string(),
        project_id.as_str().to_string(),
        book_id.as_str().to_string(),
        format.to_string(),
        "queued".to_string(),
        None,
        None,
        None,
        options_json.to_string(),
        None,
        created_at,
        None,
    )
}

/// Get one export record by id.
pub fn get_export(conn: &Connection, id: &str) -> Result<Option<ExportRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, project_id, book_id, format, status, path, bytes, error, \
             options_json, run_id, created_at, finished_at FROM exports WHERE id = ?",
        )
        .map_err(map_err)?;
    let mut rows = stmt.query(params![id]).map_err(map_err)?;
    let Some(row) = rows.next().map_err(map_err)? else {
        return Ok(None);
    };
    Ok(Some(row_to_export(
        row.get(0).map_err(map_err)?,
        row.get(1).map_err(map_err)?,
        row.get(2).map_err(map_err)?,
        row.get(3).map_err(map_err)?,
        row.get(4).map_err(map_err)?,
        row.get(5).map_err(map_err)?,
        row.get(6).map_err(map_err)?,
        row.get(7).map_err(map_err)?,
        row.get(8).map_err(map_err)?,
        row.get(9).map_err(map_err)?,
        row.get(10).map_err(map_err)?,
        row.get(11).map_err(map_err)?,
    )?))
}

/// List a project's export records, newest first.
pub fn list_exports(conn: &Connection, project_id: &ProjectId) -> Result<Vec<ExportRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, project_id, book_id, format, status, path, bytes, error, \
             options_json, run_id, created_at, finished_at FROM exports \
             WHERE project_id = ? ORDER BY updated_at DESC, id",
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
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, Option<i64>>(11)?,
            ))
        })
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (
            id,
            project,
            book,
            format,
            status,
            path,
            bytes,
            error,
            options,
            run,
            created,
            finished,
        ) = row.map_err(map_err)?;
        out.push(row_to_export(
            id, project, book, format, status, path, bytes, error, options, run, created, finished,
        )?);
    }
    Ok(out)
}

/// Transition an export record's status (terminal transitions also stamp
/// `finished_at`; `path`/`bytes`/`error` update only when `Some`).
pub fn set_export_status(
    conn: &Connection,
    id: &str,
    status: &str,
    path: Option<&str>,
    bytes: Option<i64>,
    error: Option<&str>,
) -> Result<bool> {
    let now = now_ms();
    let finished: Option<i64> = match status {
        "done" | "error" => Some(now),
        _ => None,
    };
    let changed = conn
        .execute(
            "UPDATE exports SET status = ?, \
             path = COALESCE(?, path), bytes = COALESCE(?, bytes), error = COALESCE(?, error), \
             finished_at = COALESCE(?, finished_at), updated_at = ? WHERE id = ?",
            params![status, path, bytes, error, finished, now, id],
        )
        .map_err(map_err)?;
    Ok(changed == 1)
}

/// Attach the driving job run identity to an export record.
pub fn set_export_run(conn: &Connection, id: &str, run_id: &RunId) -> Result<bool> {
    let changed = conn
        .execute(
            "UPDATE exports SET run_id = ?, updated_at = ? WHERE id = ?",
            params![run_id.as_str(), now_ms(), id],
        )
        .map_err(map_err)?;
    Ok(changed == 1)
}

/// One segment revision (immutable edit history entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRevisionRow {
    /// Row id.
    pub id: i64,
    ///Owning segment.
    pub segment_id: SegmentId,
    /// Revision kind (`translate`, `edit`, `polish`, ...).
    pub kind: String,
    /// Target before the edit (`None` = was pending).
    pub previous_target: Option<String>,
    /// Target after the edit (`None` = cleared to pending).
    pub new_target: Option<String>,
    /// Creation time (epoch ms).
    pub created_at: i64,
}

/// Load a segment's revision history, oldest first.
pub fn load_segment_history(
    conn: &Connection,
    segment_id: &SegmentId,
) -> Result<Vec<SegmentRevisionRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, segment_id, kind, previous_target, new_target, created_at \
             FROM segment_revisions WHERE segment_id = ? ORDER BY id",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![segment_id.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, segment, kind, previous, new, created) = row.map_err(map_err)?;
        out.push(SegmentRevisionRow {
            id,
            segment_id: SegmentId::new(segment)?,
            kind,
            previous_target: previous,
            new_target: new,
            created_at: created,
        });
    }
    Ok(out)
}

/// Stale-guarded segment edit: writes `target` only when the current stored
/// target equals `expected` (`None` = expected pending). A mismatch returns
/// `Error::Conflict` with the stale detail (never secrets) instead of
/// clobbering concurrent work.
pub fn edit_segment(
    conn: &Connection,
    segment_id: &SegmentId,
    target: Option<&str>,
    expected: Option<&str>,
    kind: &str,
) -> Result<()> {
    let current: Option<String> = conn
        .query_row(
            "SELECT target FROM translations WHERE segment_id = ?",
            params![segment_id.as_str()],
            |row| row.get(0),
        )
        .map_err(map_err)?;
    if current.as_deref() != expected {
        return Err(Error::Conflict(format!(
            "segment {} changed since load (expected {}, found {})",
            segment_id.as_str(),
            expected.map(|_| "set").unwrap_or("pending"),
            current.as_ref().map(|_| "set").unwrap_or("pending"),
        )));
    }
    set_target(conn, segment_id, target, kind)
}

/// Per-chapter translation counts without loading segment bodies.
/// Returns `(chapter_id, source_segments, target_words_approx, done_flag)`
/// keyed by chapter ordinal. Word counts use the same whitespace rule as
/// the service layer; computed in SQL for large books.
pub fn chapter_translation_counts(
    conn: &Connection,
    book_id: &BookId,
) -> Result<Vec<(i64, i64, i64)>> {
    let mut stmt = conn
        .prepare(
            "SELECT c.ordinal, \
                COUNT(s.id), \
                COALESCE(SUM(CASE WHEN t.target IS NOT NULL AND TRIM(t.target) != '' THEN 1 ELSE 0 END), 0) \
             FROM chapters c \
             LEFT JOIN segments s ON s.chapter_id = c.id \
             LEFT JOIN translations t ON t.segment_id = s.id \
             WHERE c.book_id = ? GROUP BY c.id ORDER BY c.ordinal",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![book_id.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(map_err)?);
    }
    Ok(out)
}

/// Whole-book word totals without loading segment bodies: `(source_words, target_words)`.
pub fn book_word_totals(conn: &Connection, book_id: &BookId) -> Result<(u64, u64)> {
    let mut stmt = conn
        .prepare(
            "SELECT s.source, t.target FROM segments s \
             JOIN chapters c ON c.id = s.chapter_id \
             LEFT JOIN translations t ON t.segment_id = s.id \
             WHERE c.book_id = ?",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![book_id.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(map_err)?;
    let mut source_words = 0u64;
    let mut target_words = 0u64;
    for row in rows {
        let (source, target) = row.map_err(map_err)?;
        source_words += source.split_whitespace().count() as u64;
        if let Some(target) = target {
            target_words += target.split_whitespace().count() as u64;
        }
    }
    Ok((source_words, target_words))
}

/// Per-chapter word totals without loading full rows: `(ordinal, source_words, target_words)`.
pub fn chapter_word_totals(conn: &Connection, book_id: &BookId) -> Result<Vec<(i64, u64, u64)>> {
    let mut stmt = conn
        .prepare(
            "SELECT c.ordinal, s.source, t.target FROM chapters c \
             LEFT JOIN segments s ON s.chapter_id = c.id \
             LEFT JOIN translations t ON t.segment_id = s.id \
             WHERE c.book_id = ? ORDER BY c.ordinal, s.ordinal",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![book_id.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(map_err)?;
    let mut out: Vec<(i64, u64, u64)> = Vec::new();
    for row in rows {
        let (ordinal, source, target) = row.map_err(map_err)?;
        if out.last().map(|entry| entry.0) != Some(ordinal) {
            out.push((ordinal, 0, 0));
        }
        let entry = out.last_mut().expect("just pushed");
        if let Some(source) = source {
            entry.1 += source.split_whitespace().count() as u64;
        }
        if let Some(target) = target {
            entry.2 += target.split_whitespace().count() as u64;
        }
    }
    Ok(out)
}

/// Recent job events across a project (newest first, capped).
pub fn recent_job_events(
    conn: &Connection,
    project_id: &ProjectId,
    limit: i64,
) -> Result<Vec<(String, i64, String, String)>> {
    let mut stmt = conn
        .prepare(
            "SELECT e.job_id, e.ts, e.kind, e.payload_json FROM job_events e \
             JOIN jobs j ON j.id = e.job_id \
             WHERE j.project_id = ? ORDER BY e.id DESC LIMIT ?",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![project_id.as_str(), limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(map_err)?);
    }
    Ok(out)
}

/// All review runs for a book (newest first by row order).
pub fn list_review_runs(conn: &Connection, book_id: &BookId) -> Result<Vec<ReviewRun>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, project_id, book_id, status, termination, reviewed_content_digest, \
             metadata_json, summary_json, issues_json, changes_json \
             FROM review_runs WHERE book_id = ? ORDER BY started_at DESC, id DESC",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![book_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
            ))
        })
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, project, book, status, termination, digest, metadata, summary, issues, changes) =
            row.map_err(map_err)?;
        out.push(ReviewRun {
            id,
            project_id: ProjectId::new(project)?,
            book_id: BookId::new(book)?,
            status,
            termination,
            reviewed_content_digest: digest,
            metadata_json: metadata,
            summary_json: summary,
            issues_json: issues,
            changes_json: changes,
        });
    }
    Ok(out)
}

/// Set one chapter's translated title.
pub fn set_chapter_title_translated(
    conn: &Connection,
    chapter_id: &ChapterId,
    title: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE chapters SET title_translated = ? WHERE id = ?",
        params![title, chapter_id.as_str()],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Recompute cached word counts for one chapter (call after segment import
/// or target writes; cheap single-chapter scan).
pub fn refresh_chapter_statistics(conn: &Connection, chapter_id: &ChapterId) -> Result<(u64, u64)> {
    let mut stmt = conn
        .prepare(
            "SELECT s.source, t.target FROM segments s \
             LEFT JOIN translations t ON t.segment_id = s.id \
             WHERE s.chapter_id = ?",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![chapter_id.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(map_err)?;
    let mut source_words = 0u64;
    let mut target_words = 0u64;
    for row in rows {
        let (source, target) = row.map_err(map_err)?;
        source_words += source.split_whitespace().count() as u64;
        if let Some(target) = target {
            target_words += target.split_whitespace().count() as u64;
        }
    }
    conn.execute(
        "INSERT INTO chapter_statistics (chapter_id, source_words, target_words, updated_at) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT(chapter_id) DO UPDATE SET source_words = excluded.source_words, \
         target_words = excluded.target_words, updated_at = excluded.updated_at",
        params![
            chapter_id.as_str(),
            source_words as i64,
            target_words as i64,
            now_ms()
        ],
    )
    .map_err(map_err)?;
    Ok((source_words, target_words))
}

/// Cached per-chapter word counts for a book: `(ordinal, source_words, target_words)`.
/// Chapters without a cache row are counted on demand and backfilled.
pub fn cached_chapter_word_totals(
    conn: &Connection,
    book_id: &BookId,
) -> Result<Vec<(i64, u64, u64)>> {
    let chapters = list_chapters(conn, book_id, 0, i64::MAX)?;
    let mut out = Vec::with_capacity(chapters.len());
    for chapter in chapters {
        let cached: Option<(i64, i64)> = conn
            .query_row(
                "SELECT source_words, target_words FROM chapter_statistics WHERE chapter_id = ?",
                params![chapter.id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();
        match cached {
            Some((source, target)) => {
                out.push((chapter.ordinal, source as u64, target as u64));
            }
            None => {
                let (source, target) = refresh_chapter_statistics(conn, &chapter.id)?;
                out.push((chapter.ordinal, source, target));
            }
        }
    }
    Ok(out)
}

/// Cached whole-book word totals: `(source_words, target_words)`.
pub fn cached_book_word_totals(conn: &Connection, book_id: &BookId) -> Result<(u64, u64)> {
    let mut source_words = 0u64;
    let mut target_words = 0u64;
    for (_, source, target) in cached_chapter_word_totals(conn, book_id)? {
        source_words += source;
        target_words += target;
    }
    Ok((source_words, target_words))
}

/// All source files for a book (import order).
pub fn list_source_files(conn: &Connection, book_id: &BookId) -> Result<Vec<SourceFile>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, book_id, filename, sha256, bytes, fmt FROM source_files \
             WHERE book_id = ? ORDER BY rowid",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![book_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, book, filename, sha256, bytes, fmt) = row.map_err(map_err)?;
        out.push(SourceFile {
            id,
            book_id: BookId::new(book)?,
            filename,
            sha256,
            bytes,
            fmt,
        });
    }
    Ok(out)
}
