//! Typed domain identities: workspace/project/book/chapter/segment/job.
//!
//! IDs are opaque strings at the boundary. Constructors validate shape so
//! invalid identities fail fast instead of corrupting paths or SQL.

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        /// Typed identity; see module docs.
        pub struct $name(String);

        impl $name {
            /// Validate shape (`[A-Za-z0-9_-]+`, 1..=64 chars).
            pub fn new(raw: impl Into<String>) -> Result<Self, crate::Error> {
                let raw = raw.into();
                let valid = !raw.is_empty()
                    && raw.len() <= 64
                    && raw
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
                if valid {
                    Ok(Self(raw))
                } else {
                    Err(crate::Error::Config(format!(concat!(
                        "invalid ", $prefix, " id"
                    ))))
                }
            }

            /// Borrow the raw identity.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(ProjectId, "project");
id_type!(BookId, "book");
id_type!(ChapterId, "chapter");
id_type!(SegmentId, "segment");
id_type!(JobId, "job");
id_type!(RunId, "run");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_legacy_hex_and_rejects_path_escape() {
        assert!(ProjectId::new("a1b2c3d4e5f60718").is_ok());
        assert!(ProjectId::new("../escape").is_err());
        assert!(ProjectId::new("").is_err());
        assert!(ProjectId::new("a".repeat(65)).is_err());
    }
}
