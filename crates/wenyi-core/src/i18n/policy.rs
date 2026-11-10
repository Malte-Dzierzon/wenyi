//! Language policy plans: profiles, prompt values, fingerprints.
//!
//! Oracle: `i18n/languages.py` + `i18n/policy/resolver.py`
//! (`_rules`/`_templates`/identity). Slice 4 covers the analysis and
//! translation phases for the book path; export/review arrive with
//! their slices.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::i18n::{normalize_language, require_language, task, TASKS};
use crate::llm::routing::canonical_json;
use crate::{Error, Result};

/// Shared honorific strategies (oracle `i18n/data/shared/honorific.json`).
static HONORIFIC: LazyLock<serde_json::Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../i18n_data/shared/honorific.json"))
        .expect("honorific JSON")
});

/// Pair overlay registry (oracle `i18n/data/pairs/registry.json`).
static PAIRS_REGISTRY: LazyLock<serde_json::Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../i18n_data/pairs/registry.json")).expect("pairs registry")
});

/// Default pair overlay (oracle `i18n/data/pairs/ja__zh.json`).
static PAIR_JA_ZH: LazyLock<serde_json::Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../i18n_data/pairs/ja__zh.json")).expect("pair JSON")
});

/// Shared prompt guidance (oracle `i18n/data/shared/guidance.json`).
static GUIDANCE: LazyLock<serde_json::Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../i18n_data/shared/guidance.json")).expect("guidance JSON")
});

/// Frozen prompt values + templates for one phase/path.
#[derive(Debug, Clone)]
pub struct PolicyPlan {
    /// Source language (`auto` allowed).
    pub source: String,
    /// Target language.
    pub target: String,
    /// Phase (`analysis` or `translation` in Slice 4).
    pub phase: String,
    /// Prompt values (sorted by key, oracle `prompt_values` order).
    pub values: Vec<(String, String)>,
    /// Frozen templates (oracle `_templates` order: TASKS filtered by group).
    pub templates: Vec<(String, String)>,
    /// Full plan fingerprint (oracle `fingerprint`).
    pub fingerprint: String,
    /// Semantic fingerprint (oracle `semantic_fingerprint`).
    pub semantic_fingerprint: String,
}

fn profile_json(code: &str) -> Result<serde_json::Value> {
    let name = match code {
        "de" => include_str!("../i18n_data/languages/de.json"),
        "en" => include_str!("../i18n_data/languages/en.json"),
        "en-GB" => include_str!("../i18n_data/languages/en-GB.json"),
        "en-US" => include_str!("../i18n_data/languages/en-US.json"),
        "es" => include_str!("../i18n_data/languages/es.json"),
        "fr" => include_str!("../i18n_data/languages/fr.json"),
        "it" => include_str!("../i18n_data/languages/it.json"),
        "ja" => include_str!("../i18n_data/languages/ja.json"),
        "ko" => include_str!("../i18n_data/languages/ko.json"),
        "pt" => include_str!("../i18n_data/languages/pt.json"),
        "pt-BR" => include_str!("../i18n_data/languages/pt-BR.json"),
        "pt-PT" => include_str!("../i18n_data/languages/pt-PT.json"),
        "ru" => include_str!("../i18n_data/languages/ru.json"),
        "vi" => include_str!("../i18n_data/languages/vi.json"),
        "zh" => include_str!("../i18n_data/languages/zh.json"),
        "zh-Hant" => include_str!("../i18n_data/languages/zh-Hant.json"),
        _ => return Err(Error::Config(format!("Unsupported language: {code:?}"))),
    };
    serde_json::from_str(name).map_err(|e| Error::Config(format!("Bad profile {code}: {e}")))
}

/// Resolve a profile with `extends` inheritance (oracle `_profile`).
pub fn profile(code: &str) -> Result<BTreeMap<String, serde_json::Value>> {
    let language = require_language(code, false)?;
    resolve_profile(&language, &mut Vec::new())
}

fn resolve_profile(
    language: &str,
    ancestors: &mut Vec<String>,
) -> Result<BTreeMap<String, serde_json::Value>> {
    if ancestors.iter().any(|a| a == language) {
        ancestors.push(language.to_string());
        return Err(Error::Config(format!(
            "Language profile inheritance cycle: {}",
            ancestors.join(" -> ")
        )));
    }
    let mut data = profile_json(language)?;
    let obj = data.as_object_mut().expect("profile object");
    let parent: Option<String> = match obj.remove("extends") {
        None => None,
        Some(serde_json::Value::String(p)) if !p.trim().is_empty() => {
            Some(require_language(&p, false)?)
        }
        Some(_) => {
            return Err(Error::Config(format!(
                "Invalid parent in language profile {language}"
            )))
        }
    };
    let mut base = match parent {
        None => BTreeMap::new(),
        Some(p) => {
            ancestors.push(language.to_string());
            let base = resolve_profile(&p, ancestors)?;
            ancestors.pop();
            base
        }
    };
    let obj = data.as_object().expect("profile object");
    for (key, value) in obj {
        if key == "policy" {
            continue;
        }
        base.insert(key.clone(), value.clone());
    }
    Ok(base)
}

fn profile_str(profile: &BTreeMap<String, serde_json::Value>, key: &str) -> String {
    profile
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Honorific rule with pair override (oracle `honorific_rule`).
pub fn honorific_rule(strategy: &str, src: &str, tgt: &str) -> String {
    let pair = format!("{}__{}", normalize_language(src), tgt);
    if PAIRS_REGISTRY.get(&pair).is_some() {
        let overlay: &serde_json::Value = match pair.as_str() {
            "ja__zh" => &PAIR_JA_ZH,
            _ => panic!("pair overlay {pair} is registered but has no implementation"),
        };
        if let Some(rule) = overlay.get("honorific").and_then(|h| h.get(strategy)) {
            if let Some(text) = rule.as_str() {
                return text.to_string();
            }
        }
    }
    HONORIFIC
        .get(strategy)
        .or_else(|| HONORIFIC.get("keep_style"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Task groups per phase (oracle `Config._policy_tasks`).
pub fn task_groups(
    phase: &str,
    book_understanding: bool,
    polish: bool,
    best_of_three: bool,
) -> Result<Vec<&'static str>> {
    match phase {
        "analysis" => Ok(if book_understanding {
            vec!["analyzer", "chapter_digest", "book_synopsis"]
        } else {
            vec!["analyzer"]
        }),
        "translation" => {
            let mut groups = if best_of_three {
                vec![
                    "precision",
                    "title_translator",
                    "glossary_extractor",
                    "glossary_history",
                ]
            } else {
                vec![
                    "translator",
                    "title_translator",
                    "glossary_extractor",
                    "glossary_history",
                ]
            };
            if polish && !best_of_three {
                groups.push("polisher");
            }
            Ok(groups)
        }
        _ => Err(Error::Config(format!(
            "Unsupported language policy phase: {phase}"
        ))),
    }
}

/// Frozen templates for task groups (oracle `_templates`, TASKS order).
fn frozen_templates(groups: &[&str]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for name in TASKS {
        if groups.iter().any(|g| name.starts_with(&format!("{g}_"))) {
            out.push((name.to_string(), task(name)?.to_string()));
        }
    }
    Ok(out)
}

/// Prompt values for a direction (oracle `_rules`).
fn rules(source: &str, target: &str, honorific_strategy: &str) -> Result<BTreeMap<String, String>> {
    let common: &serde_json::Value = &GUIDANCE;
    let target_profile = profile(target)?;
    let source_profile = if source == "auto" {
        BTreeMap::new()
    } else {
        profile(source)?
    };
    let source_label = if source == "auto" {
        "the source language".to_string()
    } else {
        let label = profile_str(&source_profile, "label");
        if label.is_empty() {
            "the source language".to_string()
        } else {
            label
        }
    };
    let source_guidance = if source == "auto" {
        common["source"].as_str().unwrap_or("").to_string()
    } else {
        let specific = profile_str(&source_profile, "source_guidance");
        if specific.is_empty() {
            common["source"].as_str().unwrap_or("").to_string()
        } else {
            specific
        }
    };
    let target_guidance = profile_str(&target_profile, "target_guidance");
    let evidence = common["evidence"].as_str().unwrap_or("").to_string();
    let default_guidance = format!(
        "{source_guidance}\n{target_guidance}\n{evidence}\n{}",
        honorific_rule("keep_style", source, target)
    );
    let configured_guidance = format!(
        "{source_guidance}\n{target_guidance}\n{evidence}\n{}",
        honorific_rule(honorific_strategy, source, target)
    );
    let metadata_template = include_str!("../i18n_data/shared/metadata_guidance.txt").to_string();
    let metadata_guidance =
        metadata_template.replace("$tgt_label", &profile_str(&target_profile, "english_name"));
    let term_guidance = format!(
        "{}{}",
        profile_str(&source_profile, "term_guidance"),
        evidence
    );
    let mut values = BTreeMap::new();
    values.insert("src_label".to_string(), source_label.clone());
    values.insert("source_language".to_string(), source_label);
    values.insert(
        "tgt_label".to_string(),
        profile_str(&target_profile, "label"),
    );
    values.insert(
        "target_language".to_string(),
        profile_str(&target_profile, "english_name"),
    );
    values.insert("lang_guidance".to_string(), default_guidance);
    values.insert("configured_lang_guidance".to_string(), configured_guidance);
    values.insert("target_guidance".to_string(), target_guidance);
    values.insert("term_guidance".to_string(), term_guidance);
    values.insert(
        "reading_field".to_string(),
        profile_str(&source_profile, "reading_field"),
    );
    values.insert(
        "punct_rule".to_string(),
        profile_str(&target_profile, "punctuation_rule"),
    );
    values.insert(
        "title_rule".to_string(),
        profile_str(&target_profile, "title_rule"),
    );
    values.insert(
        "digest_length".to_string(),
        profile_str(&target_profile, "digest_length"),
    );
    values.insert(
        "synopsis_length".to_string(),
        profile_str(&target_profile, "synopsis_length"),
    );
    values.insert(
        "review_evidence_tools".to_string(),
        include_str!("../i18n_data/shared/review_evidence_tools.txt").to_string(),
    );
    values.insert("metadata_guidance".to_string(), metadata_guidance);
    Ok(values)
}
/// Canonical JSON hash (oracle `content_hash`).
pub fn content_hash(value: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(canonical_json(value).as_bytes());
    let digest = hasher.finalize();
    crate::document::hash::hex_of(&digest)
}

/// Resolve a policy plan (oracle `resolve_policy`, analysis/translation only).
pub fn resolve_policy(
    source: &str,
    target: &str,
    phase: &str,
    groups: &[&str],
    honorific_strategy: &str,
) -> Result<PolicyPlan> {
    let source = require_language(source, true)?;
    let target = require_language(target, false)?;
    if phase != "analysis" && phase != "translation" {
        return Err(Error::Config(format!(
            "Unsupported language policy phase: {phase}"
        )));
    }
    let values = rules(&source, &target, honorific_strategy)?;
    let templates = frozen_templates(groups)?;
    let resources: Vec<(String, String)> = templates
        .iter()
        .map(|(name, text)| {
            (
                format!("tasks/{name}.txt"),
                content_hash(&serde_json::Value::String(text.clone())),
            )
        })
        .collect();
    // Slice 4 identity: ja->zh has no source/target/pair operation bindings,
    // so only prompt.language_rules is selected (verified against the oracle
    // for the ja->zh fake-config direction; other directions widen this).
    let operations = vec![serde_json::json!({
        "id": "prompt.language_rules",
        "point": "prompt.compose",
        "implementation_version": "1",
        "contract_version": 1,
        "options": [],
        "llm_operations": [],
    })];
    let effective_rules: BTreeMap<&str, &String> = {
        let joined = templates
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let mut used = used_variables(&joined);
        if templates.iter().any(|(name, _)| {
            name == "translator_system"
                || name == "review_fixer_system"
                || name == "precision_generation_system"
        }) {
            used.insert("configured_lang_guidance".to_string());
        }
        values
            .iter()
            .filter(|(key, _)| used.contains(*key))
            .map(|(key, value)| (key.as_str(), value))
            .collect()
    };
    let mut identity = serde_json::Map::new();
    identity.insert("schema_version".to_string(), serde_json::json!(1));
    identity.insert("source".to_string(), serde_json::json!(source));
    identity.insert("target".to_string(), serde_json::json!(target));
    identity.insert("phase".to_string(), serde_json::json!(phase));
    identity.insert("path".to_string(), serde_json::json!("book"));
    identity.insert("operations".to_string(), serde_json::json!(operations));
    identity.insert(
        "rules".to_string(),
        serde_json::to_value(&effective_rules).expect("rules JSON"),
    );
    identity.insert(
        "resources".to_string(),
        serde_json::to_value(
            resources
                .iter()
                .map(|(name, hash)| vec![name.clone(), hash.clone()])
                .collect::<Vec<_>>(),
        )
        .expect("resources JSON"),
    );
    if phase == "analysis" {
        // Oracle always embeds source_identity for analysis; the semantic
        // fingerprint excludes it. Slice 4 resolves with empty identity.
        identity.insert("source_identity".to_string(), serde_json::json!(""));
    }
    let fingerprint = content_hash(&serde_json::Value::Object(identity.clone()));
    // Analysis plans carry source_identity in the fingerprint only.
    let mut semantic = identity.clone();
    semantic.remove("source_identity");
    let semantic_fingerprint = content_hash(&serde_json::Value::Object(semantic));
    Ok(PolicyPlan {
        source,
        target,
        phase: phase.to_string(),
        values: values.into_iter().collect(),
        templates,
        fingerprint,
        semantic_fingerprint,
    })
}

/// `$name` / `${name}` variables referenced by templates.
fn used_variables(text: &str) -> std::collections::BTreeSet<String> {
    let mut used = std::collections::BTreeSet::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let mut j = i + 1;
            let braced = j < bytes.len() && bytes[j] == b'{';
            if braced {
                j += 1;
            }
            let start = j;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j > start && (!braced || (j < bytes.len() && bytes[j] == b'}')) {
                used.insert(text[start..j].to_string());
                i = j + usize::from(braced);
                continue;
            }
        }
        i += 1;
    }
    used
}

impl PolicyPlan {
    /// Bind a derived artifact to the templates/rules that produced it
    /// (oracle `PolicyPlan.task_fingerprint`).
    pub fn task_fingerprint(&self, prefix: &str) -> String {
        let templates: Vec<(&String, &String)> = self
            .templates
            .iter()
            .filter(|(name, _)| name.starts_with(&format!("{prefix}_")))
            .map(|(name, text)| (name, text))
            .collect();
        let joined = templates
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let used = used_variables(&joined);
        let mut values: BTreeMap<&str, &String> = self
            .values
            .iter()
            .filter(|(key, _)| used.contains(key.as_str()))
            .map(|(key, value)| (key.as_str(), value))
            .collect();
        if ["translator", "review_fixer", "precision"].contains(&prefix)
            && used.contains("lang_guidance")
        {
            if let Some(configured) = self
                .values
                .iter()
                .find(|(key, _)| key == "configured_lang_guidance")
            {
                values.insert("lang_guidance", &configured.1);
            }
        }
        content_hash(&serde_json::json!({
            "source": self.source,
            "target": self.target,
            "source_identity": "",
            "path": "book",
            "templates": templates.iter().map(|(n, t)| vec![n, t]).collect::<Vec<_>>(),
            "rules": values,
            "operations": [["prompt.language_rules", "1", 1, []]],
        }))
    }

    /// Render one template with prompt values + explicit arguments
    /// (oracle `prompts.render`).
    pub fn render(&self, name: &str, extra: &BTreeMap<String, String>) -> Result<String> {
        if !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') || name.is_empty() {
            return Err(Error::Config(format!("Invalid prompt name: {name}")));
        }
        let template = self
            .templates
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, text)| text.as_str())
            .unwrap_or_else(|| task(name).unwrap_or(""));
        if template.is_empty() && self.templates.iter().all(|(n, _)| n != name) {
            task(name)?;
        }
        let mut values: BTreeMap<&str, &str> = self
            .values
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        if [
            "translator_system",
            "review_fixer_system",
            "precision_generation_system",
        ]
        .contains(&name)
        {
            let configured = values
                .remove("configured_lang_guidance")
                .unwrap_or_default();
            values.insert("lang_guidance", configured);
        }
        substitute(template, &values, extra).map_err(|missing| {
            Error::Config(format!("Prompt {name} is missing argument: {missing}"))
        })
    }
}

/// Single-pass `string.Template` substitution: `$$` escapes, `$name` and
/// `${name}` resolve from prompt values first, then explicit arguments.
fn substitute(
    template: &str,
    values: &BTreeMap<&str, &str>,
    extra: &BTreeMap<String, String>,
) -> std::result::Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            out.push('$');
            i += 2;
            continue;
        }
        let mut j = i + 1;
        let braced = j < bytes.len() && bytes[j] == b'{';
        if braced {
            j += 1;
        }
        let start = j;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
            j += 1;
        }
        if j == start || (braced && (j >= bytes.len() || bytes[j] != b'}')) {
            // Not a placeholder: Python leaves `$` + text intact on
            // `substitute` only for `$$`; a lone `$` stays literal too
            // unless followed by an identifier.
            out.push('$');
            i += 1;
            continue;
        }
        let key = &template[start..j];
        let replacement = extra
            .get(key)
            .map(String::as_str)
            .or_else(|| values.get(key).copied());
        match replacement {
            Some(text) => out.push_str(text),
            None => return Err(key.to_string()),
        }
        i = j + usize::from(braced);
    }
    Ok(out)
}
