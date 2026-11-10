//! Token budgets. Oracle: `ingest/tokens.py` (`cl100k_base` estimator).

use std::sync::LazyLock;

/// Encoding name shared with the oracle.
pub const ENCODING_NAME: &str = "cl100k_base";

static ENCODING: LazyLock<tiktoken_rs::CoreBPE> =
    LazyLock::new(|| tiktoken_rs::cl100k_base().expect("bundled cl100k_base ranks"));

/// `cl100k_base` token count for `text` (0 for empty).
pub fn count_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    ENCODING.encode_ordinary(text).len()
}

/// Largest char-boundary prefix of `text` within `max_tokens` tokens.
/// Returns byte length; mirrors oracle `_prefix_within_tokens` (binary search
/// + whitespace backtrack, minimum 1 char handled by callers).
pub fn prefix_within_tokens(text: &str, max_tokens: usize) -> usize {
    if text.is_empty() || max_tokens == 0 {
        return 0;
    }
    if count_tokens(text) <= max_tokens {
        return text.len();
    }
    // Binary search over char boundaries.
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut low = 0usize;
    let mut high = chars.len();
    while low < high {
        let mid = low.midpoint(high) + 1;
        let end = if mid >= chars.len() {
            text.len()
        } else {
            chars[mid].0
        };
        if count_tokens(&text[..end]) <= max_tokens {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let mut end = if low >= chars.len() {
        text.len()
    } else {
        chars[low].0
    };
    // Prefer a whitespace break like the oracle (rfind over space/tab/newline).
    if end < text.len() {
        let prefix = &text[..end];
        if let Some(pos) = prefix.rfind([' ', '\t', '\n']) {
            if pos > 0 {
                end = pos;
            }
        }
    }
    end
}
