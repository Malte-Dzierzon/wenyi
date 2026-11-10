//! Diagnostic helpers: credential redaction for logs and errors.
//!
//! Oracle: `llm/credentials.py` (`CredentialRedactor`, `validate_credential`).
//! Rule: secrets are replaced longest-first; validation rejects control
//! characters before any HTTP layer can echo an invalid header.

use crate::{Error, Result};

/// Replacement marker for redacted secrets.
pub const REDACTED: &str = "[redacted]";

/// Reject secrets containing control characters.
pub fn validate_credential(secret: Option<&str>) -> Result<()> {
    if let Some(secret) = secret {
        if secret.chars().any(|c| c.is_control()) {
            return Err(Error::Config(
                "API credentials cannot contain control characters; enter the key again"
                    .to_string(),
            ));
        }
    }
    Ok(())
}

/// Redact known secrets from diagnostic text (longest first).
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    /// Build from an invocation's credential snapshot (empty entries ignored).
    pub fn new(credentials: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut secrets: Vec<String> = credentials
            .into_iter()
            .map(Into::into)
            .filter(|s| !s.is_empty())
            .collect();
        secrets.sort();
        secrets.dedup();
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        Self { secrets }
    }

    /// Replace every known secret in `value` with `[redacted]`.
    pub fn redact(&self, value: &str) -> String {
        let mut out = value.to_string();
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), REDACTED);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_canary_and_rejects_control_chars() {
        let canary = "sk-canary-9f8e7d6c5b4a";
        let redactor = Redactor::new([Some(canary), None, Some("other")].into_iter().flatten());
        let message = format!("request failed for key {canary} on connection default");
        let redacted = redactor.redact(&message);
        assert!(!redacted.contains(canary));
        assert!(redacted.contains(REDACTED));
        assert!(redacted.contains("connection default"));

        assert!(validate_credential(Some("clean-key_123")).is_ok());
        assert!(validate_credential(None).is_ok());
        assert!(validate_credential(Some("bad\nkey")).is_err());
    }

    #[test]
    fn longest_secret_wins_on_overlap() {
        let redactor = Redactor::new(["abcdef", "abc"]);
        assert_eq!(redactor.redact("key abcdef here"), "key [redacted] here");
    }
}
