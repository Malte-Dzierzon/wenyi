//! Glossary matcher parity against the pinned oracle (`matcher.json`).

use std::path::PathBuf;
use wenyi_core::glossary::{normalize_match_key, source_matches_text, TermMatcher};

fn golden() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("golden")
        .join("snapshots")
        .join("matcher.json");
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

#[test]
fn matcher_cases_match_oracle() {
    let want = golden();
    for case in want["cases"].as_array().unwrap() {
        let term = case["term"].as_str().unwrap();
        let text = case["text"].as_str().unwrap();
        let note = case["note"].as_str().unwrap();
        assert_eq!(
            normalize_match_key(term),
            case["normalized_key"].as_str().unwrap(),
            "{note} key"
        );
        assert_eq!(
            source_matches_text(term, text),
            case["matches"].as_bool().unwrap(),
            "{note} matches"
        );
        let matcher = TermMatcher::new(&[term.to_string()]);
        assert_eq!(
            matcher.first_hit(text),
            case["first_hit"].as_str(),
            "{note} first_hit"
        );
    }
}

#[test]
fn alias_matching_matches_oracle() {
    let want = golden();
    let matcher = TermMatcher::new(&[
        "Elias Vance".to_string(),
        "Elias".to_string(),
        "Keeper Vance".to_string(),
    ]);
    assert_eq!(
        matcher.first_hit("Elias tightened his coat"),
        want["aliases"]["alias_hit"].as_str()
    );
    assert_eq!(
        matcher.first_hit("Keeper Vance watched the ships"),
        want["aliases"]["source_hit"].as_str()
    );
    assert_eq!(
        matcher.first_hit("The baker prepared bread"),
        want["aliases"]["miss"].as_str()
    );
}
