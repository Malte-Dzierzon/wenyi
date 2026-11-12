//! Glossary term matching. Oracle: `glossary/store.py` (matcher half).
//!
//! NFKC + casefold normalization, ruby-marker stripping, word-boundary
//! matching for space-delimited scripts, substring matching for CJK.
//! The store half (SQLite tables) lands with the Slice 4 migration.

pub mod matcher;
pub mod store;
pub use matcher::{
    normalize_match_key, source_matches_text, CandidateIndex, TermMatcher, SOURCE_ONLY_TYPES,
};
pub use store::{
    all_terms, all_terms_counts, book_overrides, clear_override, get_term, mark_conflicts_resolved,
    open_conflicts, resolve_term, set_mentions, set_override, stats, terms_for_book, terms_in,
    upsert_term, GlossaryTerm,
};
