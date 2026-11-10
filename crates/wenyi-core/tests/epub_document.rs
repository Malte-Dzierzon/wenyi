//! End-to-end EPUB document parity against oracle dumps.

use std::path::PathBuf;
use wenyi_core::document::reader;

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
fn documents_match_oracle() {
    for name in [
        "sample",
        "nested_toc",
        "grouped_nav",
        "cross_resource",
        "degenerate",
    ] {
        let doc = reader::read_epub(&fixture(&format!("{name}.epub")), "ja", "zh").unwrap();
        let mut got = serde_json::to_value(&doc).unwrap();
        // Normalize the absolute source path like the oracle dump does.
        got["source_path"] =
            serde_json::Value::String("<abs>/".to_string() + &format!("{name}.epub"));
        // Annotation contexts are Slice 4 inputs; everything else must match.
        let mut want = load_json(&format!("{name}.document.json"));
        want["meta"]
            .as_object_mut()
            .unwrap()
            .remove("epub_annotation_contexts");
        want["meta"].as_object_mut().unwrap().remove("toc_entries");
        let got_obj = got["meta"].as_object().unwrap();
        for (key, value) in want["meta"].as_object().unwrap() {
            assert_eq!(&got_obj[key], value, "{name} meta.{key}");
        }
        assert_eq!(got["title"], want["title"], "{name} title");
        assert_eq!(got["fmt"], want["fmt"], "{name} fmt");
        let got_chapters = got["chapters"].as_array().unwrap();
        let want_chapters = want["chapters"].as_array().unwrap();
        assert_eq!(
            got_chapters.len(),
            want_chapters.len(),
            "{name} chapter count"
        );
        for (index, (got, want)) in got_chapters.iter().zip(want_chapters).enumerate() {
            assert_eq!(got["title"], want["title"], "{name} ch{index} title");
            assert_eq!(
                got["segments"], want["segments"],
                "{name} ch{index} segments"
            );
        }
    }
}

#[test]
fn peek_title_matches_reader() {
    for name in [
        "sample",
        "nested_toc",
        "grouped_nav",
        "cross_resource",
        "degenerate",
    ] {
        let path = fixture(&format!("{name}.epub"));
        let peeked = reader::peek_title(&path).unwrap();
        let doc = reader::read_epub(&path, "ja", "zh").unwrap();
        assert_eq!(peeked, doc.title, "{name}");
    }
}
