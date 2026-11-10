//! EPUB logical chapter layout. Oracle: `ingest/epub_layout.py` +
//! `ingest/epub_chapters.py` + `markup/anchors.py` fragment ops.

use std::collections::BTreeMap;

use crate::document::annotate::SEGMENT_ID_ATTR;
use crate::document::epub::TocEntry;
use crate::document::markup::parse_html;
use crate::document::models::{Chapter, Segment};
use crate::{Error, Result};

/// Strategy name for the default top-level TOC policy.
pub const STRATEGY_TOP_LEVEL_TOC: &str = "top-level-toc";
/// Strategy name for the spine fallback (one chapter per resource).
pub const STRATEGY_SPINE_FALLBACK: &str = "spine-fallback";

/// Annotated physical resource ready for layout.
#[derive(Debug, Clone)]
pub struct EpubResource {
    /// Spine order.
    pub index: usize,
    /// Zip member path.
    pub href: String,
    /// Resource title.
    pub title: String,
    /// Annotated segments.
    pub segments: Vec<Segment>,
    /// Backfill template.
    pub template: String,
    /// Fragment → segment anchor (None = after final block).
    pub fragment_anchors: BTreeMap<String, Option<String>>,
}

/// Map id/name values to segment anchors (oracle `fragment_anchor_map`).
pub fn fragment_anchor_map(template: &str) -> BTreeMap<String, Option<String>> {
    let dom = parse_html(template);
    let mut mapping: BTreeMap<String, Option<String>> = BTreeMap::new();
    for node in dom.descendants(0) {
        if dom.tag(node).is_none() {
            continue;
        }
        let mut identifiers = Vec::new();
        for key in ["id", "name"] {
            if let Some(value) = dom.attr(node, key) {
                if !value.is_empty() {
                    identifiers.push(value.to_string());
                }
            }
        }
        if identifiers.is_empty() {
            continue;
        }
        let block = if dom.has_attr(node, SEGMENT_ID_ATTR) {
            Some(node)
        } else {
            dom.find_parent(node, |p| dom.has_attr(p, SEGMENT_ID_ATTR))
        };
        let anchor = match block {
            Some(block) => dom.attr(block, SEGMENT_ID_ATTR).map(str::to_string),
            None => dom
                .find_next_with_attr(node, SEGMENT_ID_ATTR)
                .and_then(|next| dom.attr(next, SEGMENT_ID_ATTR))
                .map(str::to_string),
        };
        for value in identifiers {
            mapping.entry(value).or_insert(anchor.clone());
        }
    }
    mapping
}

/// Mutable TOC entry with layout positions attached.
#[derive(Debug, Clone)]
pub struct LocatedEntry {
    /// The parsed entry.
    pub entry: TocEntry,
    /// Resolved segment anchor (anchored boundaries only).
    pub segment_anchor: Option<String>,
    /// Position in the flattened segment stream.
    pub boundary_position: Option<usize>,
    /// Entry id this boundary was inherited from (grouping nodes).
    pub inherited_boundary_from: Option<String>,
}

/// Attach boundary positions + grouping inheritance (oracle `_logical_chapters` head).
pub fn locate_entries(resources: &[EpubResource], entries: &[TocEntry]) -> Vec<LocatedEntry> {
    let mut anchor_positions: BTreeMap<String, usize> = BTreeMap::new();
    let mut resource_starts: BTreeMap<String, usize> = BTreeMap::new();
    let mut resource_by_href: BTreeMap<&str, &EpubResource> = BTreeMap::new();
    let mut position = 0usize;
    for resource in resources {
        resource_by_href.insert(resource.href.as_str(), resource);
        resource_starts.insert(resource.href.clone(), position);
        for segment in &resource.segments {
            if let Some(anchor) = &segment.anchor {
                anchor_positions.insert(anchor.clone(), position);
            }
            position += 1;
        }
    }

    let mut located: Vec<LocatedEntry> = entries
        .iter()
        .map(|entry| LocatedEntry {
            entry: entry.clone(),
            segment_anchor: None,
            boundary_position: None,
            inherited_boundary_from: None,
        })
        .collect();

    for located in located.iter_mut() {
        let href = located.entry.resource_href.as_str();
        let Some(resource) = resource_by_href.get(href) else {
            continue;
        };
        let Some(start) = resource_starts.get(href).copied() else {
            continue;
        };
        let fragment = located.entry.fragment.as_str();
        if !fragment.is_empty() && !resource.fragment_anchors.contains_key(fragment) {
            // Invalid fragments must never fall back to the resource start.
            continue;
        }
        let segment_anchor = if fragment.is_empty() {
            resource.segments.first().and_then(|s| s.anchor.clone())
        } else {
            resource.fragment_anchors.get(fragment).cloned().flatten()
        };
        if let Some(anchor) = &segment_anchor {
            if let Some(pos) = anchor_positions.get(anchor) {
                located.segment_anchor = Some(anchor.clone());
                located.boundary_position = Some(*pos);
                continue;
            }
        }
        if !fragment.is_empty() {
            // Fragment exists after the final text block.
            located.boundary_position = Some(start + resource.segments.len());
        } else {
            // Empty title pages are valid boundaries at the stream position.
            located.boundary_position = Some(start);
        }
    }

    // Grouping inheritance: href-less nodes inherit the first resolvable child.
    let mut toc_paths: Vec<String> = Vec::new();
    for located in &located {
        let path = located.entry.toc_path.clone();
        if !path.is_empty() && !toc_paths.contains(&path) {
            toc_paths.push(path);
        }
    }
    for toc_path in &toc_paths {
        let mut children: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (index, located) in located.iter().enumerate() {
            if located.entry.toc_path != *toc_path {
                continue;
            }
            if let Some(parent) = located.entry.parent_index {
                children.entry(parent).or_default().push(index);
            }
        }
        let indices: Vec<usize> = located
            .iter()
            .enumerate()
            .filter(|(_, l)| l.entry.toc_path == *toc_path)
            .map(|(i, _)| i)
            .collect();
        for index in indices.into_iter().rev() {
            if located[index].boundary_position.is_some() {
                continue;
            }
            if !located[index].entry.raw_href.is_empty() {
                continue;
            }
            let node_index = located[index].entry.node_index;
            let descendant = children.get(&node_index).and_then(|kids| {
                kids.iter()
                    .find(|kid| located[**kid].boundary_position.is_some())
                    .copied()
            });
            if let Some(descendant) = descendant {
                located[index].boundary_position = located[descendant].boundary_position;
                located[index].inherited_boundary_from =
                    Some(located[descendant].entry.entry_id.clone());
            }
        }
    }
    located
}

/// Select top-level boundaries (oracle `TopLevelTocStrategy.select`).
fn select_top_level(entries: &[LocatedEntry]) -> Vec<usize> {
    // Indices into `entries`, deduped by position with anchor preference.
    let mut by_position: BTreeMap<usize, usize> = BTreeMap::new();
    for (index, located) in entries.iter().enumerate() {
        let Some(position) = located.boundary_position else {
            continue;
        };
        if located.entry.depth != 0 || located.entry.external {
            continue;
        }
        match by_position.get(&position) {
            None => {
                by_position.insert(position, index);
            }
            Some(previous) => {
                let previous_anchored = entries[*previous].segment_anchor.is_some();
                let current_anchored = located.segment_anchor.is_some();
                // Anchored beats unanchored; unanchored keeps the earliest.
                if current_anchored && !previous_anchored {
                    by_position.insert(position, index);
                }
            }
        }
    }
    by_position.into_values().collect()
}

/// Degenerate-TOC detection (oracle `_toc_collapsed_to_single_boundary`).
fn toc_collapsed_to_single_boundary(
    located: &[LocatedEntry],
    canonical_toc_path: &str,
    resources: &[EpubResource],
) -> bool {
    let mut top_level = 0;
    for entry in located {
        if entry.entry.toc_path != canonical_toc_path || entry.entry.depth != 0 {
            continue;
        }
        if entry.entry.external || entry.boundary_position.is_none() {
            continue;
        }
        top_level += 1;
    }
    if top_level < 2 {
        return false;
    }
    let non_empty = resources.iter().filter(|r| !r.segments.is_empty()).count();
    non_empty >= 2
}

/// Split resources into logical chapters (oracle `_logical_chapters`).
/// Returns (chapters, strategy name, canonical TOC path).
pub fn logical_chapters(
    resources: &mut [EpubResource],
    located: &[LocatedEntry],
) -> Result<(Vec<Chapter>, String, String)> {
    // Canonical TOC: first in TOC order with selectable boundaries.
    let mut ordered_toc_paths: Vec<&str> = Vec::new();
    for entry in located {
        let path = entry.entry.toc_path.as_str();
        if !path.is_empty() && !ordered_toc_paths.contains(&path) {
            ordered_toc_paths.push(path);
        }
    }
    let mut canonical = String::new();
    let mut boundaries: Vec<usize> = Vec::new();
    for toc_path in &ordered_toc_paths {
        let subset: Vec<LocatedEntry> = located
            .iter()
            .filter(|l| l.entry.toc_path == *toc_path)
            .cloned()
            .collect();
        // Map subset indices back to `located` indices.
        let subset_positions: Vec<usize> = located
            .iter()
            .enumerate()
            .filter(|(_, l)| l.entry.toc_path == *toc_path)
            .map(|(i, _)| i)
            .collect();
        let selected = select_top_level(&subset);
        if !selected.is_empty() {
            canonical = toc_path.to_string();
            boundaries = selected.into_iter().map(|i| subset_positions[i]).collect();
            break;
        }
    }
    boundaries.sort_by_key(|i| located[*i].boundary_position.unwrap_or(usize::MAX));

    if boundaries.len() == 1 && toc_collapsed_to_single_boundary(located, &canonical, resources) {
        boundaries.clear();
    }

    if boundaries.is_empty() {
        return Ok((
            spine_fallback(resources),
            STRATEGY_SPINE_FALLBACK.to_string(),
            canonical,
        ));
    }

    // Flattened segment stream with ownership info.
    let mut stream: Vec<(usize, usize)> = Vec::new(); // (resource idx, segment idx)
    for (resource_index, resource) in resources.iter().enumerate() {
        for segment_index in 0..resource.segments.len() {
            stream.push((resource_index, segment_index));
        }
    }
    let position_of = |i: usize| -> Result<usize> {
        located[i].boundary_position.ok_or_else(|| {
            Error::Config("EPUB chapter boundary is missing an integer position".to_string())
        })
    };

    let mut slices: Vec<(usize, usize, Option<usize>)> = Vec::new();
    let first = position_of(boundaries[0])?;
    if first > 0 {
        slices.push((0, first, None));
    }
    for (index, boundary) in boundaries.iter().enumerate() {
        let start = position_of(*boundary)?;
        let end = if index + 1 < boundaries.len() {
            position_of(boundaries[index + 1])?
        } else {
            stream.len()
        };
        if end > start {
            slices.push((start, end, Some(*boundary)));
        }
    }

    let mut chapters = Vec::new();
    for (start, end, boundary) in slices {
        let mut segments = Vec::new();
        for (resource_index, segment_index) in stream.iter().take(end).skip(start) {
            let (resource_index, segment_index) = (*resource_index, *segment_index);
            segments.push(resources[resource_index].segments[segment_index].clone());
        }
        for (index, segment) in segments.iter_mut().enumerate() {
            segment.index = index;
        }
        let (title, toc_entry_id, href) = match boundary {
            Some(boundary) => {
                let entry = &located[boundary];
                let fallback_href = segments
                    .first()
                    .and_then(|s| s.resource_href.clone())
                    .unwrap_or_else(|| entry.entry.resource_href.clone());
                (
                    entry.entry.title.clone(),
                    Some(entry.entry.entry_id.clone()),
                    fallback_href,
                )
            }
            None => {
                let first = &segments[0];
                let title = if first.kind == crate::document::models::KIND_HEADING {
                    first.source.clone()
                } else {
                    String::new()
                };
                (title, None, first.resource_href.clone().unwrap_or_default())
            }
        };
        let mut meta = serde_json::Map::new();
        meta.insert(
            "epub_split_strategy".to_string(),
            serde_json::Value::String(STRATEGY_TOP_LEVEL_TOC.to_string()),
        );
        if let Some(toc_entry_id) = toc_entry_id {
            meta.insert(
                "toc_entry_id".to_string(),
                serde_json::Value::String(toc_entry_id),
            );
        }
        chapters.push(Chapter {
            index: chapters.len(),
            title,
            segments,
            href: if href.is_empty() { None } else { Some(href) },
            template: None,
            meta,
        });
    }
    Ok((chapters, STRATEGY_TOP_LEVEL_TOC.to_string(), canonical))
}

/// One chapter per non-empty spine resource (oracle spine fallback).
fn spine_fallback(resources: &mut [EpubResource]) -> Vec<Chapter> {
    let mut chapters = Vec::new();
    for resource in resources {
        if resource.segments.is_empty() {
            continue;
        }
        for (index, segment) in resource.segments.iter_mut().enumerate() {
            segment.index = index;
        }
        let mut meta = serde_json::Map::new();
        meta.insert(
            "epub_split_strategy".to_string(),
            serde_json::Value::String(STRATEGY_SPINE_FALLBACK.to_string()),
        );
        chapters.push(Chapter {
            index: chapters.len(),
            title: resource.title.clone(),
            segments: std::mem::take(&mut resource.segments),
            href: Some(resource.href.clone()),
            template: None,
            meta: meta.clone(),
        });
        let _ = meta;
    }
    chapters
}
