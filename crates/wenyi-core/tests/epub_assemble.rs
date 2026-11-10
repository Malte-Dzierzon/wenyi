//! EPUB assembly parity against oracle exports.
//!
//! Compares per-member XHTML/OPF/NCX output with the oracle `assemble()`
//! exports captured in `*.exports.json`. Pure serializations (OPF/NCX via
//! BeautifulSoup `xml`) are normalized before comparison; body XHTML is
//! compared structurally (text + element skeleton), since the oracle and
//! Rust use different DOM serializers.

use std::path::PathBuf;
use wenyi_core::document::{assemble, reader};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("epub")
        .join(name)
}

fn load_json(name: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(fixture(name)).unwrap()).unwrap()
}

fn translated_doc(name: &str) -> wenyi_core::document::models::Document {
    let mut doc = reader::read_epub(&fixture(&format!("{name}.epub")), "ja", "zh").unwrap();
    // Restore oracle-only meta the Rust reader omits (Slice 4 inputs).
    let oracle = load_json(&format!("{name}.document.json"));
    for key in ["toc_entries", "epub_annotation_contexts"] {
        if let Some(value) = oracle["meta"].get(key) {
            doc.meta.insert(key.to_string(), value.clone());
        }
    }
    for entry in doc
        .meta
        .get_mut("toc_entries")
        .and_then(|v| v.as_array_mut())
        .into_iter()
        .flatten()
    {
        let title = entry
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        entry.as_object_mut().unwrap().insert(
            "title_translated".to_string(),
            serde_json::Value::String(format!("TR-{title}")),
        );
    }
    for chapter in &mut doc.chapters {
        for segment in &mut chapter.segments {
            segment.target = Some(format!(
                "T{}-{} {}",
                chapter.index, segment.index, segment.source
            ));
        }
    }
    doc
}

fn assemble_to_string(
    name: &str,
    options: &assemble::AssembleOptions,
) -> std::collections::BTreeMap<String, String> {
    let doc = translated_doc(name);
    let dir = std::env::temp_dir().join(format!(
        "wenyi-assemble-test-{}-{}",
        name,
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("out.epub");
    assemble::assemble_epub(&doc, &fixture(&format!("{name}.epub")), &out, options).unwrap();
    let file = std::fs::File::open(&out).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut members = std::collections::BTreeMap::new();
    let names: Vec<String> = archive
        .file_names()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .map(|s| s.into_owned())
        .collect();
    for member in names {
        let low = member.to_lowercase();
        if low.ends_with(".xhtml")
            || low.ends_with(".html")
            || low.ends_with(".htm")
            || low.ends_with(".opf")
            || low.ends_with(".ncx")
        {
            let mut entry = archive.by_name(&member).unwrap();
            let mut raw = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut raw).unwrap();
            members.insert(member, String::from_utf8(raw).unwrap());
        }
    }
    std::fs::remove_dir_all(&dir).ok();
    members
}

/// Structural skeleton: tag/attr/text sequence ignoring attribute order,
/// quoting, void-element style, and XML declarations.
fn skeleton(html: &str) -> String {
    let dom = wenyi_core::document::markup::parse_html(html);
    let mut parts = Vec::new();
    for node in dom.descendants(0) {
        match &dom.nodes[node] {
            wenyi_core::document::markup::Node::Element { tag, attrs, .. } => {
                let mut sorted: Vec<(&str, &str)> =
                    attrs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                sorted.sort();
                parts.push(format!("<{tag} {sorted:?}>"));
            }
            wenyi_core::document::markup::Node::Text { text }
                // Skip whitespace-only nodes: XML vs HTML serializers format
                // inter-element whitespace differently (content still matches).
                if !text.trim().is_empty() =>
            {
                parts.push(format!("T{text}"));
            }
            _ => {}
        }
    }
    parts.join("\n")
}

fn check_mode(name: &str, mode: &str, options: &assemble::AssembleOptions) {
    let want = load_json(&format!("{name}.exports.json"));
    let got_members = assemble_to_string(name, options);
    let want_members = want[mode]["members"].as_object().unwrap();
    assert_eq!(
        got_members.len(),
        want_members.len(),
        "{name}:{mode} member count"
    );
    for (member, want_text) in want_members {
        let got_text = got_members
            .get(member)
            .unwrap_or_else(|| panic!("{name}:{mode} missing {member}"));
        let want_text = want_text.as_str().unwrap();
        // OPF/NCX and body XHTML all compare structurally: the oracle and
        // Rust use different DOM serializers (whitespace, quoting, void
        // style), so byte equality is neither expected nor desired.
        assert_eq!(
            skeleton(got_text),
            skeleton(want_text),
            "{name}:{mode} {member}"
        );
    }
}

#[test]
fn exports_match_oracle() {
    let mono = assemble::AssembleOptions::monolingual();
    let bi = assemble::AssembleOptions {
        bilingual: true,
        order: "target_first".to_string(),
        preserve_source_style: false,
        preserve_source_ruby: None,
    };
    let bi_src = assemble::AssembleOptions {
        bilingual: true,
        order: "source_first".to_string(),
        preserve_source_style: false,
        preserve_source_ruby: None,
    };
    let bi_style = assemble::AssembleOptions {
        bilingual: true,
        order: "target_first".to_string(),
        preserve_source_style: true,
        preserve_source_ruby: None,
    };
    for name in [
        "sample",
        "nested_toc",
        "grouped_nav",
        "cross_resource",
        "degenerate",
    ] {
        check_mode(name, "mono", &mono);
        check_mode(name, "bi", &bi);
        check_mode(name, "bi_src", &bi_src);
        check_mode(name, "bi_style", &bi_style);
    }
}

#[test]
fn cjk_toc_titles_survive_without_mojibake() {
    // The oracle mojibakes CJK TOC rewrites in declaration-less NAV docs
    // (UnicodeDammit mis-detects its own UTF-8 output as cp949). The Rust
    // port decodes UTF-8 losslessly and must keep translated titles intact.
    let mut doc = translated_doc("grouped_nav");
    for entry in doc
        .meta
        .get_mut("toc_entries")
        .and_then(|v| v.as_array_mut())
        .into_iter()
        .flatten()
    {
        let title = entry
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        entry.as_object_mut().unwrap().insert(
            "title_translated".to_string(),
            serde_json::Value::String(format!("译{title}")),
        );
    }
    let dir = std::env::temp_dir().join(format!("wenyi-cjk-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("out.epub");
    assemble::assemble_epub(
        &doc,
        &fixture("grouped_nav.epub"),
        &out,
        &assemble::AssembleOptions::monolingual(),
    )
    .unwrap();
    let file = std::fs::File::open(&out).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut nav = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("OEBPS/nav.xhtml").unwrap(), &mut nav)
        .unwrap();
    for title in ["译PART I", "译Section 1", "译PART II", "译Section 2"] {
        assert!(nav.contains(title), "NAV lost {title}: {nav}");
    }
    std::fs::remove_dir_all(&dir).ok();
}
