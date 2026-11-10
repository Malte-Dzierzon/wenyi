//! High-level EPUB reader. Oracle: `ingest/epub_reader.py`.

use std::fs::File;
use std::path::Path;
use zip::ZipArchive;

use crate::document::annotate::{annotate_resource, INLINE_META_KEY};
use crate::document::epub::{decode_markup, find_opf_path, parse_opf, parse_toc_entries};
use crate::document::layout::{
    fragment_anchor_map, locate_entries, logical_chapters, EpubResource,
};
use crate::document::models::Document;
use crate::Result;

/// Read the book title without annotating resources (oracle `peek_epub_title`).
pub fn peek_title(path: &Path) -> Result<String> {
    let file = File::open(path).map_err(|e| crate::Error::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut archive = ZipArchive::new(file)
        .map_err(|e| crate::Error::Config(format!("Invalid EPUB zip: {e}")))?;
    let opf_path = find_opf_path(&mut archive)?;
    let package = parse_opf(&mut archive, &opf_path)?;
    if package.title.is_empty() {
        Ok(path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string())
    } else {
        Ok(package.title)
    }
}

/// Read spine resources and build logical chapters (oracle `read_epub`).
///
/// Note: annotation *contexts* (`epub_annotation_contexts`) are translation-
/// pipeline inputs built at Slice 4; the segment-level `epub_annotations` meta
/// recorded here is their complete input.
pub fn read_epub(path: &Path, source_lang: &str, target_lang: &str) -> Result<Document> {
    let file = File::open(path).map_err(|e| crate::Error::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut archive = ZipArchive::new(file)
        .map_err(|e| crate::Error::Config(format!("Invalid EPUB zip: {e}")))?;
    let names: std::collections::BTreeSet<String> = archive
        .file_names()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| crate::Error::Config(format!("Invalid EPUB zip: {e}")))?
        .into_iter()
        .map(|s| s.into_owned())
        .collect();
    let opf_path = find_opf_path(&mut archive)?;
    let package = parse_opf(&mut archive, &opf_path)?;
    let entries = parse_toc_entries(&mut archive, &package.toc_paths);

    let mut resources: Vec<EpubResource> = Vec::new();
    for (resource_index, href) in package.spine_hrefs.iter().enumerate() {
        if !names.contains(href) {
            continue;
        }
        let mut entry = archive.by_name(href).map_err(|e| {
            crate::Error::Config(format!("Invalid EPUB: cannot read {href:?}: {e}"))
        })?;
        let mut raw = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut raw).map_err(|e| crate::Error::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let html = decode_markup(&raw);
        let (title, segments, template) = annotate_resource(
            &html,
            resource_index,
            href,
            &package.title,
            package.toc_paths.contains(href),
        );
        let fragment_anchors = fragment_anchor_map(&template);
        resources.push(EpubResource {
            index: resource_index,
            href: href.clone(),
            title,
            segments,
            template,
            fragment_anchors,
        });
    }

    let located = locate_entries(&resources, &entries);
    let (mut chapters, strategy, split_toc_path) = logical_chapters(&mut resources, &located)?;

    // Strip templates + inline meta (rebuilt deterministically at export).
    for chapter in &mut chapters {
        chapter.template = None;
        for segment in &mut chapter.segments {
            segment.meta.remove(INLINE_META_KEY);
        }
    }

    let book_title = if package.title.is_empty() {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    } else {
        package.title.clone()
    };
    let mut meta = serde_json::Map::new();
    meta.insert("epub_schema".to_string(), serde_json::Value::from(5));
    meta.insert("opf_path".to_string(), serde_json::Value::String(opf_path));
    meta.insert(
        "toc_paths".to_string(),
        serde_json::Value::Array(
            package
                .toc_paths
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    meta.insert(
        "epub_resources".to_string(),
        serde_json::Value::Array(
            resources
                .iter()
                .map(|r| serde_json::json!({"index": r.index, "href": r.href}))
                .collect(),
        ),
    );
    meta.insert(
        "epub_split_strategy".to_string(),
        serde_json::Value::String(strategy),
    );
    meta.insert(
        "epub_split_toc_path".to_string(),
        serde_json::Value::String(split_toc_path),
    );

    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    Ok(Document {
        title: book_title,
        source_lang: source_lang.to_string(),
        target_lang: target_lang.to_string(),
        fmt: "epub".to_string(),
        source_path: absolute.to_string_lossy().into_owned(),
        chapters,
        meta,
    })
}
