//! Ruby reading markers. Oracle: `markup/ruby.py`.
//!
//! The oracle regex `〘[^〙]*〙` strips balanced pairs; an unterminated
//! `〘...` (no closing `〙`) is kept verbatim.

/// Strip ruby reading markers (oracle `strip_ruby_markers`).
pub fn strip_markers(text: &str) -> String {
    if !text.contains('〘') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('〘') {
        out.push_str(&rest[..start]);
        match rest[start..].find('〙') {
            Some(end) => rest = &rest[start + end + '〙'.len_utf8()..],
            None => {
                rest = &rest[start..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::strip_markers;

    #[test]
    fn strips_balanced_pair() {
        assert_eq!(strip_markers("漢〘かん〙字"), "漢字");
    }

    #[test]
    fn strips_multiple_pairs() {
        assert_eq!(strip_markers("漢〘かん〙字〘じ〙"), "漢字");
    }

    #[test]
    fn strips_nested_by_first_close() {
        // Oracle `〘[^〙]*〙`: match ends at the first `〙`.
        assert_eq!(strip_markers("a〘b〘c〙d〙e"), "ad〙e");
    }

    #[test]
    fn keeps_unterminated_verbatim() {
        assert_eq!(strip_markers("漢〘かん字"), "漢〘かん字");
    }

    #[test]
    fn leaves_plain_text_untouched() {
        assert_eq!(strip_markers("plain text"), "plain text");
    }
}
