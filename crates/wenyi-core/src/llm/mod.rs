//! LLM providers, routing, retries, budgets, usage. Oracle: `llm/`.
//!
//! Deterministic core (operations, routing, retry classification, usage
//! accounting) with model calls isolated behind one provider trait.
//! Single-owner retry: this module owns backoff; adapters never retry.

pub mod client;
pub mod compat;
pub mod errors;
pub mod fake;
pub mod json;
pub mod limits;
pub mod operations;
pub mod providers;
pub mod routing;
pub mod transport;
pub mod usage;
