//! Document processing: hashing, token budgets, models, readers, segmentation.
//!
//! Oracle: `ingest/` + `markup/` + `assemble/` (readers/writers) in the Python
//! reference. Deterministic throughout; model calls happen at the edges.

pub mod anchors;
pub mod annotate;
pub mod assemble;
pub mod docx;
pub mod epub;
pub mod hash;
pub mod html;
pub mod layout;
pub mod markup;
pub mod models;
pub mod reader;
pub mod ruby;
pub mod segment;
pub mod srt;
pub mod text;
pub mod tokens;
