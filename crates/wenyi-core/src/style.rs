//! Series style memory: versioned profiles, book overrides, proposals.
//!
//! The oracle keeps one mutable `analysis.json` per book and only ever fills
//! missing keys (`refresh_analysis`). Slice 5 replaces that with versioned,
//! evidence-backed memory scoped correctly:
//!
//! - One immutable [`StyleProfile`] version series per project + language
//!   pair holds the author's stable voice.
//! - Each book may pin overrides to one profile version for deliberate
//!   narrator/POV/genre shifts; overrides never leak into sibling books.
//! - Translation batches receive one consistent snapshot (profile version +
//!   merged brief) for the whole run; new versions activate at safe
//!   boundaries only.
//!
//! Merge rules preserve oracle semantics: the profile payload keeps the
//! oracle analysis shape (rendered by `Analyzer::style_brief`), and
//! additive-only fills still apply when seeding from a book analysis.

use rusqlite::Connection;

use crate::model::{BookId, ProjectId};
use crate::pipeline_agents::Analyzer;
use crate::{store, Result};

/// One consistent style snapshot for a translation run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleSnapshot {
    /// Profile version in force (`0` = none published yet).
    pub profile_version: i64,
    /// Rendered brief (profile + book overrides merged).
    pub brief: String,
}

/// Merge profile JSON with book overrides (override keys win).
pub fn merge_profile(profile_json: &str, overrides_json: Option<&str>) -> serde_json::Value {
    let mut merged: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(profile_json).unwrap_or_default();
    if let Some(text) = overrides_json {
        let overrides: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(text).unwrap_or_default();
        for (key, value) in overrides {
            merged.insert(key, value);
        }
    }
    serde_json::Value::Object(merged)
}

/// Render the merged brief with the oracle brief renderer.
pub fn render_snapshot(profile_json: &str, overrides_json: Option<&str>) -> String {
    match merge_profile(profile_json, overrides_json) {
        serde_json::Value::Object(map) => Analyzer::style_brief(&map),
        _ => String::new(),
    }
}

/// Load the book's style snapshot: latest profile + pinned overrides.
///
/// Overrides pinned to a stale version are ignored (the book opted into an
/// older voice explicitly; a re-pin is an explicit action, not silent drift).
pub fn snapshot_for_book(
    conn: &Connection,
    project_id: &ProjectId,
    book_id: &BookId,
    source_lang: &str,
    target_lang: &str,
) -> Result<StyleSnapshot> {
    let profile = store::latest_style_profile(conn, project_id, source_lang, target_lang)?;
    let Some(profile) = profile else {
        return Ok(StyleSnapshot {
            profile_version: 0,
            brief: String::new(),
        });
    };
    let pinned = store::load_book_style_override(conn, book_id, source_lang, target_lang)?;
    let overrides = match pinned {
        Some(held) if held.profile_version == profile.version => Some(held.overrides_json),
        _ => None,
    };
    Ok(StyleSnapshot {
        profile_version: profile.version,
        brief: render_snapshot(&profile.profile_json, overrides.as_deref()),
    })
}

/// Seed the series profile from a finished book analysis.
///
/// First book publishes version 1. Later books only fill keys the profile
/// is missing (additive-only, oracle `refresh_analysis` semantics); when at
/// least one key was filled, a new version is published with the book as
/// evidence. Existing profile keys never change here — refinements go
/// through proposals.
pub fn seed_from_analysis(
    conn: &Connection,
    project_id: &ProjectId,
    book_id: &BookId,
    source_lang: &str,
    target_lang: &str,
    analysis: &serde_json::Map<String, serde_json::Value>,
    prompt_version: &str,
) -> Result<store::StyleProfile> {
    let current = store::latest_style_profile(conn, project_id, source_lang, target_lang)?;
    let Some(current) = current else {
        let profile_json = serde_json::Value::Object(analysis.clone()).to_string();
        let evidence = serde_json::json!([{
            "book_id": book_id.as_str(),
            "kind": "initial_analysis",
        }])
        .to_string();
        return store::publish_style_profile(
            conn,
            project_id,
            source_lang,
            target_lang,
            &profile_json,
            &evidence,
            prompt_version,
        );
    };
    let mut merged: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&current.profile_json).unwrap_or_default();
    let mut filled = Vec::new();
    for (key, value) in analysis {
        if !merged.contains_key(key) {
            merged.insert(key.clone(), value.clone());
            filled.push(key.clone());
        }
    }
    if filled.is_empty() {
        return Ok(current);
    }
    let profile_json = serde_json::Value::Object(merged).to_string();
    let evidence = serde_json::json!([{
        "book_id": book_id.as_str(),
        "kind": "seed_fill",
        "filled": filled,
    }])
    .to_string();
    store::publish_style_profile(
        conn,
        project_id,
        source_lang,
        target_lang,
        &profile_json,
        &evidence,
        prompt_version,
    )
}

/// Accept one pending proposal: merge `after` into a new profile version.
///
/// The proposal's `before` must still match the live profile value —
/// otherwise the proposal is stale and the accept is rejected (`Ok(None)`).
/// On success the proposal flips to `accepted`.
pub fn accept_proposal(
    conn: &Connection,
    project_id: &ProjectId,
    source_lang: &str,
    target_lang: &str,
    proposal_id: i64,
) -> Result<Option<store::StyleProfile>> {
    let proposals = store::pending_style_proposals(conn, project_id, source_lang, target_lang)?;
    let Some(proposal) = proposals.into_iter().find(|item| item.id == proposal_id) else {
        return Ok(None);
    };
    let current = store::latest_style_profile(conn, project_id, source_lang, target_lang)?;
    let Some(current) = current else {
        return Ok(None);
    };
    let mut merged: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&current.profile_json).unwrap_or_default();
    let live = merged
        .get(&proposal.property)
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let before: serde_json::Value = serde_json::from_str(&proposal.before_json).unwrap_or_default();
    if live != before {
        return Ok(None);
    }
    let after: serde_json::Value = serde_json::from_str(&proposal.after_json).unwrap_or_default();
    merged.insert(proposal.property.clone(), after);
    let profile_json = serde_json::Value::Object(merged).to_string();
    let evidence = serde_json::json!([{
        "proposal_id": proposal.id,
        "property": proposal.property,
        "reason": proposal.reason,
        "evidence": serde_json::from_str::<serde_json::Value>(&proposal.evidence_json)
            .unwrap_or(serde_json::Value::Null),
    }])
    .to_string();
    let published = store::publish_style_profile(
        conn,
        project_id,
        source_lang,
        target_lang,
        &profile_json,
        &evidence,
        &proposal.prompt_version,
    )?;
    store::set_style_proposal_status(conn, proposal.id, "accepted")?;
    Ok(Some(published))
}
