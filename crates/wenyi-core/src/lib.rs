#![forbid(unsafe_code)]

//! Wenyi translation engine core.
//!
//! Domain, store, documents, LLM, pipeline, and jobs. This crate must never
//! depend on GPUI, UI types, HTTP server frameworks, or the desktop app layer.

pub mod config;
pub mod curator;
pub mod document;
pub mod error;
pub mod export;
pub mod glossary;
pub mod i18n;
pub mod jobs;
pub mod llm;
pub mod model;
pub mod pipeline;
pub mod pipeline_agents;
pub mod redact;
pub mod review;
pub mod review_autofix;
pub mod review_conflicts;
pub mod review_evidence;
pub mod review_loop;
pub mod review_models;
pub mod service;
pub mod store;
pub mod style;
pub use error::{Error, Result};
