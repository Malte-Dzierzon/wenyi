//! Language resources: registry, profiles, prompt templates.
//!
//! Oracle: `i18n/` (registry, language profiles, policy resolver `_rules`,
//! prompt renderer). Templates are embedded with `include_str!` so builds
//! stay hermetic; a byte-parity test guards against silent drift.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::{Error, Result};

pub mod policy;

/// Supported language codes in registry order.
pub const LANGUAGES: [&str; 16] = [
    "zh", "en", "ja", "ko", "fr", "de", "es", "it", "pt", "ru", "vi", "zh-Hant", "en-US", "en-GB",
    "pt-BR", "pt-PT",
];

/// Alias table source (oracle `languages/registry.json`), parsed once.
const REGISTRY_JSON: &str = include_str!("../i18n_data/languages/registry.json");

/// Parsed alias registry.
static ALIASES: LazyLock<serde_json::Value> =
    LazyLock::new(|| serde_json::from_str(REGISTRY_JSON).expect("registry JSON"));

/// Prompt task templates (oracle `i18n/data/tasks/*.txt`).
macro_rules! tasks {
    ($($name:ident),*) => {
        /// All embedded task template names (oracle `i18n/policy/tasks.py::TASKS`).
        pub const TASKS: [&str; 34] = [$(stringify!($name)),*];
        /// Look up one embedded task template by name.
        pub fn task(name: &str) -> Result<&'static str> {
            match name {
                $(stringify!($name) => Ok(include_str!(concat!("../i18n_data/tasks/", stringify!($name), ".txt"))),)*
                _ => Err(Error::Config(format!("Unknown prompt task: {name}"))),
            }
        }
    };
}

tasks!(
    analyzer_system,
    analyzer_user,
    book_synopsis_system,
    book_synopsis_user,
    chapter_digest_system,
    chapter_digest_user,
    glossary_extractor_system,
    glossary_extractor_user,
    glossary_history_system,
    glossary_history_user,
    language_detector_system,
    polisher_continue_user,
    polisher_system,
    polisher_user,
    precision_generation_system,
    precision_packet,
    precision_synthesize,
    precision_translate,
    review_agent_system,
    review_agent_user,
    review_arbiter_system,
    review_arbiter_user,
    review_fixer_system,
    review_fixer_user,
    reviewer_system,
    reviewer_user,
    srt_batch_system,
    srt_batch_user,
    srt_single_system,
    srt_single_user,
    title_translator_system,
    title_translator_user,
    translator_system,
    translator_user
);

/// Normalize a language code through the registry aliases (oracle
/// `normalize_language`); empty for unknown/auto.
pub fn normalize_language(code: &str) -> String {
    let normalized = code.trim().to_lowercase().replace('_', "-");
    ALIASES["aliases"]
        .get(&normalized)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Validate a code (oracle `require_language`).
pub fn require_language(code: &str, allow_auto: bool) -> Result<String> {
    if allow_auto && (code.is_empty() || code.trim().to_lowercase() == "auto") {
        return Ok("auto".to_string());
    }
    let language = normalize_language(code);
    if language.is_empty() {
        return Err(Error::Config(format!(
            "Unsupported language: {code:?}; choose: {}",
            LANGUAGES.join(" / ")
        )));
    }
    Ok(language)
}

/// Parsed alias map for golden tests.
pub fn alias_map() -> BTreeMap<String, String> {
    ALIASES["aliases"]
        .as_object()
        .expect("aliases object")
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
        .collect()
}
