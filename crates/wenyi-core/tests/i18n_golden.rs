//! i18n golden parity: embedded templates, prompt values, fingerprints.
//!
//! The oracle pins `main@39068fcc`; fingerprints below were verified
//! against `resolve_policy` for the ja->zh fake-config direction.

use std::collections::BTreeMap;
use std::path::PathBuf;
use wenyi_core::i18n::policy;

fn oracle_task(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("packages")
        .join("core")
        .join("wenyi_core")
        .join("i18n")
        .join("data")
        .join("tasks")
        .join(format!("{name}.txt"));
    std::fs::read_to_string(&path).unwrap()
}

#[test]
fn embedded_templates_match_oracle_bytes() {
    for name in wenyi_core::i18n::TASKS {
        let embedded = wenyi_core::i18n::task(name).unwrap();
        assert_eq!(embedded, oracle_task(name), "{name} drifted from oracle");
    }
}

#[test]
fn registry_aliases_match_oracle() {
    let map = wenyi_core::i18n::alias_map();
    assert_eq!(map.get("jp").map(String::as_str), Some("ja"));
    assert_eq!(map.get("zh-hans").map(String::as_str), Some("zh"));
    assert_eq!(map.get("zh-tw").map(String::as_str), Some("zh-Hant"));
    assert_eq!(map.get("pt-br").map(String::as_str), Some("pt-BR"));
    assert_eq!(wenyi_core::i18n::normalize_language("JP"), "ja");
    assert_eq!(wenyi_core::i18n::normalize_language("xx"), "");
    assert_eq!(
        wenyi_core::i18n::require_language("auto", true).unwrap(),
        "auto"
    );
    assert!(wenyi_core::i18n::require_language("xx", false).is_err());
}

fn plan(phase: &str) -> policy::PolicyPlan {
    let groups = policy::task_groups(phase, true, true, false).unwrap();
    let refs: Vec<&str> = groups.iter().map(|s| s as &str).collect();
    policy::resolve_policy("ja", "zh", phase, &refs, "keep_style").unwrap()
}

#[test]
fn policy_fingerprints_match_oracle() {
    let analysis = plan("analysis");
    assert_eq!(
        analysis.fingerprint,
        "bb852227387c860b2f8d049cf3e7f0df7cae573bc70d9e37c5b82ef745b7026f"
    );
    assert_eq!(
        analysis.semantic_fingerprint,
        "2c8247f521d4d83afb2f72ab4b8afceee228db4a8051757d4b07e9d08be2dea9"
    );
    assert_eq!(
        analysis.task_fingerprint("analyzer"),
        "6df4dd3e5efeadbefad26c8d2fc048212f0a33d82025037a5700f93629935ad2"
    );
    assert_eq!(
        analysis.task_fingerprint("chapter_digest"),
        "90825b8b523b43aa7fb31bc7ce22d856bee0d77d54bdf4647269335574f70c0c"
    );
    assert_eq!(
        analysis.task_fingerprint("book_synopsis"),
        "269d3e525520588176c4efcb3e70e1b751b762d69197ca76233b4f3db1d25246"
    );
    let translation = plan("translation");
    assert_eq!(
        translation.fingerprint,
        "b845652d6ab9f5fd401aa919b415ce4a2c7e0512f172eefc54274261aec7c385"
    );
    assert_eq!(
        translation.task_fingerprint("translator"),
        "15e688b31cebe9b97787e31fe6265882ddf7f2c95322e2ecd36ff9f6c5dfb05e"
    );
    assert_eq!(
        translation.task_fingerprint("polisher"),
        "5f9a6540d4b9bbcd51d934ebeaaa4d6b23b1bb1516350f06525defd813c99871"
    );
    assert_eq!(
        translation.task_fingerprint("title_translator"),
        "af00743c768bc9d18728876d40af719d60dd5d9000150697f21ae713ec2c5619"
    );
    assert_eq!(
        translation.task_fingerprint("glossary_extractor"),
        "b6ba450a43b6b4dce1ab7acad119c332682d934865527beeca8696201253fc6a"
    );
    assert_eq!(
        translation.task_fingerprint("glossary_history"),
        "abbb82619fcae925a6e729fa238730eabf8fd07746cd5ee7aef8027af793abca"
    );
}

#[test]
fn rendered_translator_system_matches_oracle() {
    let translation = plan("translation");
    let got = translation
        .render("translator_system", &BTreeMap::new())
        .unwrap();
    // Oracle `prompts.render('translator_system', src='ja', tgt='zh')` head.
    assert!(got.starts_with(
        "You are an experienced literary translator, translating Japanese fiction \
         into Simplified Chinese,"
    ));
    // Configured guidance (pair honorific override) replaces the default.
    assert!(got.contains("先輩 → 前辈"));
    assert!(!got.contains("$lang_guidance"));
}

#[test]
fn missing_arguments_fail_loudly() {
    let translation = plan("translation");
    let err = translation
        .render("translator_user", &BTreeMap::new())
        .unwrap_err();
    assert!(err.to_string().contains("missing argument"));
}

#[test]
fn every_registered_pair_has_an_implemented_overlay() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("i18n_data")
        .join("pairs")
        .join("registry.json");
    let registry: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let pairs: Vec<String> = registry.as_object().unwrap().keys().cloned().collect();
    assert!(!pairs.is_empty(), "pair registry must not be empty");
    for pair in pairs {
        let (src, tgt) = pair.split_once("__").expect("pair key is src__tgt");
        // Exercises the overlay path; a registered pair without an
        // implementation panics instead of silently falling back to Null.
        let _ = policy::honorific_rule("keep_style", src, tgt);
    }
}
