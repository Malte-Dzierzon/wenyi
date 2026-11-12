//! Glossary persistence. Oracle: `glossary/store.py` (store half).
//!
//! Terms live project-wide per language pair (Slice 4: one book per
//! project, so project == book scope). `upsert_term` never replaces an
use std::collections::BTreeMap;

use rusqlite::{params, Connection};

use crate::glossary::matcher::{match_text, sources_match_normalized, SOURCE_ONLY_TYPES};
use crate::model::ProjectId;
use crate::store::{map_err, now_ms};
use crate::Result;

/// One glossary term (oracle `GlossaryTerm`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlossaryTerm {
    /// Source term (match key).
    pub source: String,
    /// Established target rendering.
    pub target: String,
    /// Kana reading (Japanese source only).
    pub reading: String,
    /// Term type (`person`, `term`, ...).
    pub term_type: String,
    /// Character gender (`male`/`female`/empty).
    pub gender: String,
    /// Alternative source spellings.
    pub aliases: Vec<String>,
    /// First chapter index where seen.
    pub first_chapter: Option<i64>,
    /// Human note.
    pub note: String,
    /// `ok` or `conflict`.
    pub status: String,
    /// Curator occurrence count.
    pub mentions: i64,
}

impl GlossaryTerm {
    /// Minimal term with oracle defaults.
    pub fn new(source: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            reading: String::new(),
            term_type: "term".to_string(),
            gender: String::new(),
            aliases: Vec::new(),
            first_chapter: None,
            note: String::new(),
            status: "ok".to_string(),
            mentions: 0,
        }
    }

    /// Normalize type/gender (oracle `__post_init__`).
    pub fn normalized(mut self) -> Self {
        self.term_type = normalize_term_type(&self.term_type);
        self.gender = normalize_gender(&self.gender);
        self
    }
}

/// Normalize a model-supplied type (oracle `normalize_term_type`).
pub fn normalize_term_type(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "term".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Normalize a model-supplied gender (oracle `normalize_gender`).
pub fn normalize_gender(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed == "unknown" {
        String::new()
    } else {
        trimmed.to_string()
    }
}

/// Source spellings allowed for matching (oracle `term_match_sources`).
pub fn term_match_sources(term: &GlossaryTerm) -> Vec<String> {
    if SOURCE_ONLY_TYPES.contains(&term.term_type.as_str()) {
        vec![term.source.clone()]
    } else {
        let mut out = vec![term.source.clone()];
        out.extend(term.aliases.iter().cloned());
        out
    }
}

/// Filter prefetched terms by occurrence in text (oracle `terms_in`).
pub fn terms_in(terms: &[GlossaryTerm], text: &str) -> Vec<GlossaryTerm> {
    let normalized = match_text(text);
    terms
        .iter()
        .filter(|term| sources_match_normalized(&term_match_sources(term), &normalized))
        .cloned()
        .collect()
}

fn row_to_term(row: &rusqlite::Row) -> rusqlite::Result<GlossaryTerm> {
    let aliases_json: String = row.get(5)?;
    let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
    Ok(GlossaryTerm {
        source: row.get(0)?,
        target: row.get(1)?,
        reading: row.get(2)?,
        term_type: row.get(3)?,
        gender: row.get(4)?,
        aliases,
        first_chapter: row.get(6)?,
        note: row.get(7)?,
        status: row.get(8)?,
        mentions: row.get(9)?,
    })
}

const SELECT_COLUMNS: &str =
    "source, target, reading, type, gender, aliases_json, first_chapter, note, status, mentions";

/// Look up one term by exact source.
pub fn get_term(
    conn: &Connection,
    project: &ProjectId,
    source_lang: &str,
    target_lang: &str,
    source: &str,
) -> Result<Option<GlossaryTerm>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM glossary_terms \
             WHERE project_id = ? AND source_lang = ? AND target_lang = ? AND source = ?"
        ))
        .map_err(map_err)?;
    let mut rows = stmt
        .query_map(
            params![project.as_str(), source_lang, target_lang, source],
            row_to_term,
        )
        .map_err(map_err)?;
    match rows.next() {
        None => Ok(None),
        Some(row) => Ok(Some(row.map_err(map_err)?)),
    }
}

/// All terms in insertion order (oracle `all_terms`; stable prompt order).
pub fn all_terms(
    conn: &Connection,
    project: &ProjectId,
    source_lang: &str,
    target_lang: &str,
) -> Result<Vec<GlossaryTerm>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM glossary_terms \
             WHERE project_id = ? AND source_lang = ? AND target_lang = ? ORDER BY id"
        ))
        .map_err(map_err)?;
    let rows = stmt
        .query_map(
            params![project.as_str(), source_lang, target_lang],
            row_to_term,
        )
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(map_err)?);
    }
    Ok(out)
}

/// Insert or update a term; return `inserted`/`unchanged`/`conflict`.
pub fn upsert_term(
    conn: &Connection,
    project: &ProjectId,
    source_lang: &str,
    target_lang: &str,
    term: &GlossaryTerm,
    chapter: Option<i64>,
) -> Result<String> {
    let existing = get_term(conn, project, source_lang, target_lang, &term.source)?;
    let now = now_ms();
    match existing {
        None => {
            conn.execute(
                "INSERT INTO glossary_terms (project_id, source_lang, target_lang, source, target, \
                 reading, type, gender, aliases_json, first_chapter, note, status, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    project.as_str(),
                    source_lang,
                    target_lang,
                    term.source,
                    term.target,
                    term.reading,
                    term.term_type,
                    term.gender,
                    serde_json::to_string(&term.aliases).expect("aliases JSON"),
                    term.first_chapter.or(chapter),
                    term.note,
                    term.status,
                    now,
                ],
            )
            .map_err(map_err)?;
            Ok("inserted".to_string())
        }
        Some(current) if current.target == term.target => {
            let mut merged = current.aliases.clone();
            merged.extend(term.aliases.iter().cloned());
            merged.sort();
            merged.dedup();
            conn.execute(
                "UPDATE glossary_terms SET reading = COALESCE(NULLIF(?, ''), reading), \
                 gender = COALESCE(NULLIF(?, ''), gender), aliases_json = ?, \
                 note = COALESCE(NULLIF(?, ''), note), updated_at = ? \
                 WHERE project_id = ? AND source_lang = ? AND target_lang = ? AND source = ?",
                params![
                    term.reading,
                    term.gender,
                    serde_json::to_string(&merged).expect("aliases JSON"),
                    term.note,
                    now,
                    project.as_str(),
                    source_lang,
                    target_lang,
                    term.source,
                ],
            )
            .map_err(map_err)?;
            Ok("unchanged".to_string())
        }
        Some(current) => {
            conn.execute(
                "INSERT INTO glossary_conflicts (project_id, source_lang, target_lang, source, \
                 existing_target, proposed_target, chapter, note, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    project.as_str(),
                    source_lang,
                    target_lang,
                    term.source,
                    current.target,
                    term.target,
                    chapter,
                    term.note,
                    now,
                ],
            )
            .map_err(map_err)?;
            conn.execute(
                "UPDATE glossary_terms SET status = 'conflict', updated_at = ? \
                 WHERE project_id = ? AND source_lang = ? AND target_lang = ? AND source = ?",
                params![now, project.as_str(), source_lang, target_lang, term.source],
            )
            .map_err(map_err)?;
            Ok("conflict".to_string())
        }
    }
}

/// Resolve a term's final translation and clear its conflicts.
pub fn resolve_term(
    conn: &Connection,
    project: &ProjectId,
    source_lang: &str,
    target_lang: &str,
    source: &str,
    target: &str,
) -> Result<bool> {
    let changed = conn
        .execute(
            "UPDATE glossary_terms SET target = ?, status = 'ok', updated_at = ? \
             WHERE project_id = ? AND source_lang = ? AND target_lang = ? AND source = ?",
            params![
                target,
                now_ms(),
                project.as_str(),
                source_lang,
                target_lang,
                source
            ],
        )
        .map_err(map_err)?;
    if changed == 0 {
        return Ok(false);
    }
    mark_conflicts_resolved(conn, project, source_lang, target_lang, source)?;
    Ok(true)
}

/// Mark every unresolved conflict for a source as handled.
pub fn mark_conflicts_resolved(
    conn: &Connection,
    project: &ProjectId,
    source_lang: &str,
    target_lang: &str,
    source: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE glossary_conflicts SET resolved = 1 \
         WHERE project_id = ? AND source_lang = ? AND target_lang = ? AND source = ?",
        params![project.as_str(), source_lang, target_lang, source],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Store the curator's occurrence count (no status change).
pub fn set_mentions(
    conn: &Connection,
    project: &ProjectId,
    source_lang: &str,
    target_lang: &str,
    source: &str,
    count: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE glossary_terms SET mentions = ?, updated_at = ? \
         WHERE project_id = ? AND source_lang = ? AND target_lang = ? AND source = ?",
        params![
            count.max(0),
            now_ms(),
            project.as_str(),
            source_lang,
            target_lang,
            source
        ],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Conflicts awaiting human resolution in occurrence order.
pub fn open_conflicts(
    conn: &Connection,
    project: &ProjectId,
    source_lang: &str,
    target_lang: &str,
) -> Result<Vec<BTreeMap<String, serde_json::Value>>> {
    use std::collections::BTreeMap;
    let mut stmt = conn
        .prepare(
            "SELECT source, existing_target, proposed_target, chapter, note, created_at \
             FROM glossary_conflicts WHERE project_id = ? AND source_lang = ? \
             AND target_lang = ? AND resolved = 0 ORDER BY created_at",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![project.as_str(), source_lang, target_lang], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        let (source, existing, proposed, chapter, note, created) = row.map_err(map_err)?;
        let mut map = BTreeMap::new();
        map.insert("source".to_string(), serde_json::Value::String(source));
        map.insert(
            "existing_target".to_string(),
            serde_json::Value::String(existing),
        );
        map.insert(
            "proposed_target".to_string(),
            serde_json::Value::String(proposed),
        );
        map.insert(
            "chapter".to_string(),
            chapter.map(serde_json::Value::from).unwrap_or_default(),
        );
        map.insert("note".to_string(), serde_json::Value::String(note));
        map.insert("created_at".to_string(), serde_json::Value::from(created));
        out.push(map);
    }
    Ok(out)
}

/// Glossary + open-conflict counts.
pub fn stats(
    conn: &Connection,
    project: &ProjectId,
    source_lang: &str,
    target_lang: &str,
) -> Result<(i64, i64)> {
    let terms: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM glossary_terms \
             WHERE project_id = ? AND source_lang = ? AND target_lang = ?",
            params![project.as_str(), source_lang, target_lang],
            |row| row.get(0),
        )
        .map_err(map_err)?;
    let conflicts: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM glossary_conflicts \
             WHERE project_id = ? AND source_lang = ? AND target_lang = ? AND resolved = 0",
            params![project.as_str(), source_lang, target_lang],
            |row| row.get(0),
        )
        .map_err(map_err)?;
    Ok((terms, conflicts))
}

/// Total term count across all language pairs (sweep gate).
pub fn all_terms_counts(conn: &Connection, project: &ProjectId) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM glossary_terms WHERE project_id = ?",
        params![project.as_str()],
        |row| row.get(0),
    )
    .map_err(map_err)
}

/// Set a book-scoped target override. The shared project term keeps its
/// mapping; the override wins for this book only.
pub fn set_override(
    conn: &Connection,
    book: &crate::model::BookId,
    source_lang: &str,
    target_lang: &str,
    source: &str,
    target: &str,
    note: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO glossary_overrides \
         (book_id, source_lang, target_lang, source, target, note, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(book_id, source_lang, target_lang, source) \
         DO UPDATE SET target = excluded.target, note = excluded.note, \
         updated_at = excluded.updated_at",
        params![
            book.as_str(),
            source_lang,
            target_lang,
            source,
            target,
            note,
            now_ms()
        ],
    )
    .map_err(map_err)?;
    Ok(())
}

/// Clear one book-scoped override; true when a row existed.
pub fn clear_override(
    conn: &Connection,
    book: &crate::model::BookId,
    source_lang: &str,
    target_lang: &str,
    source: &str,
) -> Result<bool> {
    let removed = conn
        .execute(
            "DELETE FROM glossary_overrides \
             WHERE book_id = ? AND source_lang = ? AND target_lang = ? AND source = ?",
            params![book.as_str(), source_lang, target_lang, source],
        )
        .map_err(map_err)?;
    Ok(removed > 0)
}

/// Load all overrides for a book as `(source, target)` pairs.
pub fn book_overrides(
    conn: &Connection,
    book: &crate::model::BookId,
    source_lang: &str,
    target_lang: &str,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn
        .prepare(
            "SELECT source, target FROM glossary_overrides \
             WHERE book_id = ? AND source_lang = ? AND target_lang = ? ORDER BY id",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![book.as_str(), source_lang, target_lang], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(map_err)?);
    }
    Ok(out)
}

/// Shared terms with book overrides applied (override target + note win;
/// base metadata otherwise). Order stays insertion-stable.
pub fn terms_for_book(
    conn: &Connection,
    project: &ProjectId,
    book: &crate::model::BookId,
    source_lang: &str,
    target_lang: &str,
) -> Result<Vec<GlossaryTerm>> {
    let mut terms: BTreeMap<String, GlossaryTerm> =
        all_terms(conn, project, source_lang, target_lang)?
            .into_iter()
            .map(|term| (term.source.clone(), term))
            .collect();
    for (source, target) in book_overrides(conn, book, source_lang, target_lang)? {
        match terms.get_mut(&source) {
            Some(term) => {
                term.target = target;
                term.note = format!("book override ({})", book.as_str());
            }
            None => {
                let mut term = GlossaryTerm::new(source.clone(), target);
                term.note = format!("book override ({})", book.as_str());
                terms.insert(source, term);
            }
        }
    }
    Ok(terms.into_values().collect())
}
