//! Config fixture parity against the Python oracle.
//!
//! Fixtures in `tests/fixtures/config_*.yaml`; expectations in
//! `config_expected.json` (generated from the oracle; see PLAN.md Slice 1).
//!
//! YAML parser decision (spike 2026-11-10): `serde_yaml 0.9` and
//! `serde-saphyr 2.1` parsed all fixtures identically. `serde_yaml` won on
//! smaller dependency weight (shares indexmap/hashbrown; saphyr pulls
//! granit-parser/zmij/annotate-snippets). Re-evaluate if serde_yaml is
//! yanked or a YAML 1.2 feature gap appears; the fixtures make swapping safe.

use std::path::PathBuf;
use wenyi_core::config;

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap()
}

#[test]
fn good_fixtures_match_oracle() {
    let expected: serde_json::Value =
        serde_json::from_str(&fixture("config_expected.json")).unwrap();
    for name in ["config_full", "config_minimal"] {
        let text = fixture(&format!("{name}.yaml"));
        let cfg = config::load(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        let want = &expected[name];
        assert_eq!(
            cfg.source_lang,
            want["source_lang"].as_str().unwrap(),
            "{name}"
        );
        assert_eq!(
            cfg.target_lang,
            want["target_lang"].as_str().unwrap(),
            "{name}"
        );
        assert_eq!(
            cfg.segment.max_tokens_per_batch,
            want["segment"]["max_tokens_per_batch"].as_i64().unwrap(),
            "{name}"
        );
        assert_eq!(
            cfg.segment.max_tokens_per_segment,
            want["segment"]["max_tokens_per_segment"].as_i64().unwrap(),
            "{name}"
        );
        assert_eq!(
            cfg.pipeline.review,
            want["pipeline"]["review"].as_bool().unwrap(),
            "{name}"
        );
        assert_eq!(
            cfg.pipeline.polish,
            want["pipeline"]["polish"].as_bool().unwrap(),
            "{name}"
        );
        // Full pipeline/output field parity, not just the sampled keys.
        let pipeline_json = serde_json::to_value(&cfg.pipeline).unwrap();
        for (key, value) in want["pipeline"].as_object().unwrap() {
            let got = &pipeline_json[key];
            assert_eq!(got, value, "{name} pipeline.{key}: {got:?} != {value:?}");
        }
        let mut tiers: Vec<(&str, &str)> = cfg
            .llm
            .tiers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        tiers.sort_unstable();
        let mut want_tiers: Vec<(&str, &str)> = want["llm_tiers"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str().unwrap()))
            .collect();
        want_tiers.sort_unstable();
        assert_eq!(tiers, want_tiers, "{name} tiers");
        let mut providers: Vec<&str> = cfg.llm.providers.keys().map(String::as_str).collect();
        providers.sort_unstable();
        let mut want_providers: Vec<&str> = want["llm_providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        want_providers.sort_unstable();
        assert_eq!(providers, want_providers, "{name} providers");
    }
}

#[test]
fn bad_fixtures_reject_like_oracle() {
    for (name, marker) in [
        ("config_bad_section", "Unknown configuration sections"),
        ("config_bad_mode", "best_of_three"),
        ("config_bad_env", "environment variable name"),
    ] {
        let text = fixture(&format!("{name}.yaml"));
        let error = config::load(&text).expect_err(&format!("{name} unexpectedly accepted"));
        assert!(
            error.to_string().contains(marker),
            "{name}: {error:?} lacks {marker:?}"
        );
    }
}

#[test]
fn root_config_yaml_parses() {
    // The shipped default must always load under the Rust parser.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("config.yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    let cfg = config::load(&text).unwrap();
    assert_eq!(cfg.source_lang, "auto");
    assert_eq!(cfg.target_lang, "zh");
}
