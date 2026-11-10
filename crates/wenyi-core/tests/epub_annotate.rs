//! XHTML annotation parity against oracle resource dumps.
//!
//! Compares (title, segments incl. meta) per spine resource. Templates are
//! compared structurally (anchor placement + segment ids), not byte-exact:
//! the oracle serializes via BeautifulSoup while Rust uses its own DOM.

use std::fs::File;
use std::path::PathBuf;
use wenyi_core::document::{annotate, epub};
use zip::ZipArchive;

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

#[test]
fn resources_match_oracle() {
    for name in [
        "sample",
        "nested_toc",
        "grouped_nav",
        "cross_resource",
        "degenerate",
    ] {
        let file = File::open(fixture(&format!("{name}.epub"))).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let opf_path = epub::find_opf_path(&mut archive).unwrap();
        let package = epub::parse_opf(&mut archive, &opf_path).unwrap();
        let want_resources = load_json(&format!("{name}.resources.json"));
        let want_resources = want_resources.as_array().unwrap();
        assert_eq!(
            package.spine_hrefs.len(),
            want_resources.len(),
            "{name} resource count"
        );
        for (index, href) in package.spine_hrefs.iter().enumerate() {
            let mut entry = archive.by_name(href).unwrap();
            let mut raw = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut raw).unwrap();
            let html = epub::decode_markup(&raw);
            let (title, segments, template) = annotate::annotate_resource(
                &html,
                index,
                href,
                &package.title,
                package.toc_paths.contains(href),
            );
            let want = &want_resources[index];
            assert_eq!(
                title,
                want["title"].as_str().unwrap(),
                "{name}:{href} title"
            );
            let got = serde_json::to_value(&segments).unwrap();
            assert_eq!(got, want["segments"], "{name}:{href} segments");
            // Template structural check: same anchors in the same order.
            let got_anchors = anchor_ids(&template);
            let want_anchors = anchor_ids(want["template"].as_str().unwrap());
            assert_eq!(got_anchors, want_anchors, "{name}:{href} template anchors");
        }
    }
}

fn anchor_ids(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(pos) = rest.find("data-tn-id") {
        rest = &rest[pos + 11..];
        let rest2 = rest.trim_start_matches(['=', ' ', '"', '\'']);
        let end = rest2.find(['"', '\'']).unwrap_or(rest2.len());
        out.push(rest2[..end].to_string());
        rest = &rest2[end..];
    }
    out
}
