#![forbid(unsafe_code)]

//! Wenyi translation engine core.
//!
//! Domain, store, documents, LLM, pipeline, and jobs. This crate must never
//! depend on GPUI, UI types, HTTP server frameworks, or the desktop app layer.

pub mod config;
pub mod document;
pub mod error;
pub mod glossary;
pub mod i18n;
pub mod jobs;
pub mod llm;
pub mod model;
pub mod pipeline;
pub mod pipeline_agents;
pub mod redact;
pub mod store;
pub use error::{Error, Result};
