//! Typed engine errors. Secrets must never be formatted into these.

use std::path::PathBuf;

/// Engine result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// All errors producible by `wenyi-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// SQLite store failure (message only; never includes bound secrets).
    #[error("store: {0}")]
    Store(String),
    /// Configuration parse/validation failure.
    #[error("config: {0}")]
    Config(String),
    /// Requested entity was not found.
    #[error("not found: {kind} {id}")]
    NotFound {
        /// Entity kind (project, book, chapter, ...).
        kind: &'static str,
        /// Entity identity.
        id: String,
    },
    /// Filesystem failure.
    #[error("io: {path}: {message}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// OS error summary.
        message: String,
    },
    /// Model provider failure (safe classification only; never bodies/secrets).
    #[error("provider: {0}")]
    Provider(String),
    /// Budget/cancellation stop; completed work remains resumable.
    #[error("stopped: {0}")]
    Stopped(String),
}
