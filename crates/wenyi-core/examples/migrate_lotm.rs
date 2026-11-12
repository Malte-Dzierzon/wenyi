//! One-shot migrator: old Python-desktop project -> new Rust workspace.
//!
//! READ-ONLY on the source (`?mode=ro`). Writes a FRESH workspace dir only.
//! Usage: `migrate_lotm <old-project-dir> <new-workspace-dir> <pid>`
//!
//! Migrates: catalog row, chapters + segments + targets (+ polish baselines),
//! chapter statuses, digests, analysis, glossary terms + conflicts, segment
//! revisions, usage/timing summaries (as artifacts), source file record.
//! Skips: jobs (re-runnable), live events (ephemeral), EPUB template bytes
//! (kept as chapter meta for future export work).

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{params, Connection};
use wenyi_core::glossary::store::{upsert_term, GlossaryTerm};
use wenyi_core::model::{BookId, ChapterId, ProjectId, SegmentId};
use wenyi_core::{store, Error};

fn open_ro(path: &Path) -> Result<Connection, Error> {
    let uri = format!(
        "file:{}?mode=ro",
        path.display().to_string().replace(' ', "%20")
    );
    Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| Error::Store(format!("open ro {}: {e}", path.display())))
}

fn artifact(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM artifacts WHERE key = ?",
        params![key],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

fn parse_dt_ms(text: &str) -> i64 {
    // Best-effort RFC3339 -> epoch ms without new deps (ordering only).
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 14 {
        let year: i64 = digits[0..4].parse().unwrap_or(1970);
        let month: i64 = digits[4..6].parse().unwrap_or(1);
        let day: i64 = digits[6..8].parse().unwrap_or(1);
        let hour: i64 = digits[8..10].parse().unwrap_or(0);
        let min: i64 = digits[10..12].parse().unwrap_or(0);
        let sec: i64 = digits[12..14].parse().unwrap_or(0);
        let days = (year - 1970) * 365 + (year - 1969) / 4 + (month - 1) * 30 + (day - 1);
        return ((days * 86400 + hour * 3600 + min * 60 + sec) * 1000).max(0);
    }
    0
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: migrate_lotm <old-project-dir> <new-workspace-dir> <new-pid>");
        std::process::exit(2);
    }
    let old_dir = Path::new(&args[1]);
    let new_dir = Path::new(&args[2]);
    let new_pid = args[3].clone();

    // Fresh target only: never merge into an existing workspace.
    let db_path = store::db_path(new_dir);
    if db_path.exists() {
        eprintln!("refusing: target db already exists: {}", db_path.display());
        std::process::exit(3);
    }

    let old_state = old_dir.join("state.sqlite3");
    let src = open_ro(&old_state).map_err(|e| format!("source: {e}"))?;

    // Catalog (for name + langs + status + source identity).
    let catalog_path = old_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|ws| ws.join("local.sqlite3"));
    let (name, src_lang, tgt_lang, status, source_sha, source_name) = match catalog_path {
        Some(path) if path.exists() => {
            let cat = open_ro(&path).map_err(|e| format!("catalog: {e}"))?;
            let doc: String = cat
                .query_row("SELECT document FROM projects LIMIT 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|e| format!("catalog row: {e}"))?;
            let doc: serde_json::Value = serde_json::from_str(&doc)?;
            let get = |k: &str| {
                doc.get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let source_path = get("source_path");
            let source_name = source_path
                .rsplit('/')
                .next()
                .unwrap_or("source")
                .to_string();
            (
                get("name"),
                get("source_lang"),
                get("target_lang"),
                get("status"),
                get("source_sha256"),
                source_name,
            )
        }
        _ => (
            "Migrated".to_string(),
            "en".to_string(),
            "de".to_string(),
            "prepared".to_string(),
            String::new(),
            "source".to_string(),
        ),
    };

    let manifest: serde_json::Value =
        serde_json::from_str(&artifact(&src, "manifest.json").ok_or("missing manifest.json")?)?;
    let empty = Vec::new();
    let chapters = manifest
        .get("chapters")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    let title = manifest
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or(&name);

    // Open target (fresh DB + migrations) via the service path.
    let service = wenyi_core::service::Service::open(new_dir).map_err(|e| format!("open: {e}"))?;
    drop(service);
    let dst = store::open(&db_path).map_err(|e| format!("dst open: {e}"))?;

    // Project + book + source file.
    let pid = ProjectId::new(new_pid.clone()).map_err(|e| format!("pid: {e}"))?;
    let row = store::create_project(&dst, &pid, &name).map_err(|e| format!("create: {e}"))?;
    store::save_artifact(
        &dst,
        &pid,
        "languages",
        &serde_json::json!({"source_lang": src_lang, "target_lang": tgt_lang}).to_string(),
    )
    .map_err(|e| format!("langs: {e}"))?;
    store::save_artifact(
        &dst,
        &pid,
        "migration",
        &serde_json::json!({"from": old_dir.display().to_string(), "status": status}).to_string(),
    )
    .map_err(|e| format!("migration note: {e}"))?;
    let book_id = BookId::new(format!("{new_pid}-b0")).map_err(|e| format!("book id: {e}"))?;
    let book =
        store::create_book(&dst, &book_id, &pid, 0, title).map_err(|e| format!("book: {e}"))?;
    // Source file bytes: copy the original into the new workspace files dir.
    let files_dir = new_dir.join("files");
    std::fs::create_dir_all(&files_dir)?;
    let mut file_bytes: i64 = 0;
    for entry in std::fs::read_dir(old_dir)? {
        let entry = entry?;
        let fname = entry.file_name().to_string_lossy().to_string();
        if fname.starts_with("source-") {
            let bytes = std::fs::read(entry.path())?;
            file_bytes = bytes.len() as i64;
            std::fs::write(files_dir.join(&fname), &bytes)?;
            store::insert_source_file(
                &dst,
                &format!("{new_pid}-s0"),
                &book.id,
                &fname,
                &source_sha,
                file_bytes,
                manifest
                    .get("fmt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("epub"),
            )
            .map_err(|e| format!("source file: {e}"))?;
        }
    }
    let _ = (row, source_name);

    // Chapters + segments + targets.
    let mut seg_index: BTreeMap<(i64, i64), SegmentId> = BTreeMap::new();
    let mut total_segments = 0i64;
    let mut total_targets = 0i64;
    for chapter in chapters {
        let ordinal = chapter.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
        let ch_title = chapter.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let ch_status = chapter
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending");
        let ch_id =
            ChapterId::new(format!("{new_pid}-c{ordinal}")).map_err(|e| format!("ch id: {e}"))?;
        let ch_row = store::create_chapter(&dst, &ch_id, &book.id, ordinal, ch_title)
            .map_err(|e| format!("chapter {ordinal}: {e}"))?;
        if ch_status != "pending" {
            store::set_chapter_status(&dst, &ch_row.id, ch_status)
                .map_err(|e| format!("status: {e}"))?;
        }
        let key = format!("chapters/ch{ordinal}.json");
        let Some(text) = artifact(&src, &key) else {
            eprintln!("warning: missing {key}, skipping");
            continue;
        };
        let doc: serde_json::Value = serde_json::from_str(&text)?;
        // Chapter extras: href, toc id, template/meta, translated title.
        let href = doc.get("href").and_then(|v| v.as_str()).unwrap_or("");
        let meta = doc
            .get("meta")
            .cloned()
            .unwrap_or(serde_json::Value::Object(Default::default()));
        let toc = meta
            .get("toc_entry_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let title_t = doc.get("title_translated").and_then(|v| v.as_str());
        dst.execute(
            "UPDATE chapters SET href = ?, toc_entry_id = ?, meta_json = ? WHERE id = ?",
            params![href, toc, meta.to_string(), ch_row.id.as_str()],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        if let Some(title_t) = title_t {
            dst.execute(
                "UPDATE chapters SET title_translated = ? WHERE id = ?",
                params![title_t, ch_row.id.as_str()],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        }
        if let Some(digest) = meta.get("source_digest").and_then(|v| v.as_str()) {
            let complete = meta
                .get("source_digest_complete")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let policy = meta
                .get("source_digest_policy")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            store::save_digest(&dst, &ch_row.id, digest, complete, policy)
                .map_err(|e| format!("digest: {e}"))?;
        }
        let empty_segs = Vec::new();
        let segments = doc
            .get("segments")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty_segs);
        // Batch insert sources first (atomic), then targets + kinds + metas.
        let batch: Vec<(SegmentId, String)> = segments
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let sid = SegmentId::new(format!("{new_pid}-s{ordinal}-{i}")).expect("seg id");
                seg_index.insert((ordinal, i as i64), sid.clone());
                (
                    sid,
                    s.get("source")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                )
            })
            .collect();
        store::insert_segments(&dst, &ch_row.id, &batch)
            .map_err(|e| format!("segments {ordinal}: {e}"))?;
        for (i, s) in segments.iter().enumerate() {
            let sid = &batch[i].0;
            let kind = s.get("kind").and_then(|v| v.as_str()).unwrap_or("text");
            let anchor = s.get("anchor").and_then(|v| v.as_str()).unwrap_or("");
            let resource = s
                .get("resource_href")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let cont = s.get("cont").and_then(|v| v.as_bool()).unwrap_or(false) as i64;
            let smeta = s.get("meta").cloned().unwrap_or(serde_json::Value::Null);
            dst.execute(
                "UPDATE segments SET kind = ?, anchor = ?, resource_href = ?, cont = ?, meta_json = ? WHERE id = ?",
                params![kind, anchor, resource, cont, smeta.to_string(), sid.as_str()],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
            let target = s
                .get("target")
                .and_then(|v| v.as_str())
                .filter(|t| !t.is_empty());
            let baseline = s.get("target_before_polish").and_then(|v| v.as_str());
            if let Some(target) = target {
                total_targets += 1;
                match baseline.filter(|b| !b.is_empty()) {
                    Some(base) => store::set_target_with_baseline(
                        &dst,
                        sid,
                        Some(target),
                        Some(base),
                        "translation",
                    ),
                    None => store::set_target(&dst, sid, Some(target), "translation"),
                }
                .map_err(|e| format!("target {ordinal}/{i}: {e}"))?;
            }
            total_segments += 1;
        }
        println!(
            "chapter {ordinal} {ch_title:?}: {} segments",
            segments.len()
        );
    }

    // Analysis + context + usage/timing (summaries as artifacts).
    if let Some(text) = artifact(&src, "analysis.json") {
        let value: serde_json::Value = serde_json::from_str(&text)?;
        let synopsis = value
            .get("book_synopsis")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        store::save_analysis(&dst, &book.id, &text).map_err(|e| format!("analysis: {e}"))?;
        if !synopsis.is_empty() {
            // Keep the synopsis discoverable on the book row as well.
            dst.execute(
                "UPDATE books SET title = ? WHERE id = ?",
                params![title, book.id.as_str()],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        }
    }
    for key in [
        "context.json",
        "usage.json",
        "timing.json",
        "report.json",
        "curator_state.json",
    ] {
        if let Some(text) = artifact(&src, key) {
            store::save_artifact(&dst, &pid, &format!("legacy/{key}"), &text)
                .map_err(|e| format!("{key}: {e}"))?;
        }
    }
    // Manifest meta (EPUB resources / TOC) for future export work.
    if let Some(meta) = manifest.get("meta") {
        store::save_artifact(&dst, &pid, "legacy/manifest_meta.json", &meta.to_string())
            .map_err(|e| format!("meta: {e}"))?;
    }

    // Glossary terms.
    let mut terms = 0i64;
    let mut stmt = src
        .prepare("SELECT source, target, reading, type, gender, aliases, first_chapter, note, status, mentions FROM glossary")
        .map_err(|e| Error::Store(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, i64>(9)?,
            ))
        })
        .map_err(|e| Error::Store(e.to_string()))?;
    for row in rows {
        let (
            source,
            target,
            reading,
            term_type,
            gender,
            aliases,
            first_chapter,
            note,
            status,
            mentions,
        ) = row.map_err(|e| Error::Store(e.to_string()))?;
        let aliases: Vec<String> = serde_json::from_str(&aliases).unwrap_or_default();
        let term = GlossaryTerm {
            source,
            target,
            reading,
            term_type,
            gender,
            aliases,
            first_chapter,
            note,
            status,
            mentions,
        };
        upsert_term(&dst, &pid, &src_lang, &tgt_lang, &term, None)
            .map_err(|e| format!("term: {e}"))?;
        terms += 1;
    }

    // Term conflicts (direct insert: preserve resolved flags).
    let mut conflicts = 0i64;
    let mut stmt = src
        .prepare("SELECT source, existing_target, proposed_target, chapter, note, resolved, created_at FROM term_conflicts")
        .map_err(|e| Error::Store(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, f64>(6)?,
            ))
        })
        .map_err(|e| Error::Store(e.to_string()))?;
    for row in rows {
        let (source, existing, proposed, chapter, note_opt, resolved, created_at) =
            row.map_err(|e| Error::Store(e.to_string()))?;
        let note = note_opt.unwrap_or_default();
        let created_ms = (created_at * 1000.0) as i64;
        dst.execute(
            "INSERT INTO glossary_conflicts (project_id, source_lang, target_lang, source, existing_target, proposed_target, chapter, note, resolved, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![pid.as_str(), src_lang, tgt_lang, source, existing, proposed, chapter, note, resolved, created_ms],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        conflicts += 1;
    }

    // `set_target` above wrote one revision row per target; drop those so the
    // legacy history below is the only history (no duplicates).
    dst.execute("DELETE FROM segment_revisions", [])
        .map_err(|e| Error::Store(e.to_string()))?;
    // Segment revisions (map (chapter, segment) -> new segment id).
    let mut revisions = 0i64;
    let mut skipped = 0i64;
    let mut stmt = src
        .prepare("SELECT chapter_index, segment_index, kind, previous_target, new_target, created_at FROM segment_revisions ORDER BY id")
        .map_err(|e| Error::Store(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|e| Error::Store(e.to_string()))?;
    for row in rows {
        let (ci, si, kind, before, after, created_at) =
            row.map_err(|e| Error::Store(e.to_string()))?;
        let Some(sid) = seg_index.get(&(ci, si)) else {
            skipped += 1;
            continue;
        };
        dst.execute(
            "INSERT INTO segment_revisions (segment_id, kind, previous_target, new_target, created_at) VALUES (?, ?, ?, ?, ?)",
            params![sid.as_str(), kind, before, after, parse_dt_ms(&created_at)],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        revisions += 1;
    }

    println!("migration done: {total_segments} segments, {total_targets} targets, {terms} terms, {conflicts} conflicts, {revisions} revisions ({skipped} skipped), source bytes {file_bytes}");
    Ok(())
}
