//! Glossary term matching. Oracle: `glossary/store.py` (matcher half).
//!
//! NFKC + casefold normalization, ruby-marker stripping, word-boundary
//! matching for space-delimited scripts, substring matching for CJK.
//! The store half (SQLite tables) lands with the Slice 4 migration.

pub mod matcher;
pub mod store;
pub use matcher::{normalize_match_key, source_matches_text, TermMatcher, SOURCE_ONLY_TYPES};
pub use store::{
    all_terms, get_term, open_conflicts, resolve_term, set_mentions, stats, terms_in, upsert_term,
    GlossaryTerm,
};
