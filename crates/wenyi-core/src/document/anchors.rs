//! EPUB href resolution and fragment lookup. Oracle: `markup/anchors.py`.

/// Resolved EPUB hyperlink: raw value plus normalized zip member + fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEpubHref {
    /// Original href value, byte-preserved.
    pub raw_href: String,
    /// Normalized ZIP member path ("" when external or empty).
    pub resource_href: String,
    /// Percent-decoded fragment ("" when absent).
    pub fragment: String,
    /// True when the href has a scheme or host.
    pub external: bool,
}

impl ResolvedEpubHref {
    /// Stable content-destination key (`resource#fragment` or `resource`).
    pub fn target_key(&self) -> String {
        if self.resource_href.is_empty() {
            return String::new();
        }
        if self.fragment.is_empty() {
            self.resource_href.clone()
        } else {
            format!("{}#{}", self.resource_href, self.fragment)
        }
    }
}

/// Percent-decode, preserving literal `+` (oracle `unquote` semantics).
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 {
            let hex = &value[i + 1..];
            if hex.len() >= 2 {
                if let Ok(byte) = u8::from_str_radix(&hex[..2], 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Split an href into (path, fragment) at the first `#`.
fn split_fragment(href: &str) -> (&str, &str) {
    match href.find('#') {
        Some(pos) => (&href[..pos], &href[pos + 1..]),
        None => (href, ""),
    }
}

/// Detect scheme/host presence (oracle `urlsplit` external check).
fn is_external(path: &str) -> bool {
    // Scheme: ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":" — but a bare
    // `a:b` path segment is rare in EPUBs; match urlsplit behavior for the
    // common `scheme://host` and `scheme:...` shapes plus protocol-relative.
    if let Some(colon) = path.find(':') {
        let scheme = &path[..colon];
        if !scheme.is_empty()
            && scheme
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        {
            // Exclude Windows drive letters (single char + not followed by //).
            if !(scheme.len() == 1 && !path[colon + 1..].starts_with('/')) {
                return true;
            }
        }
    }
    path.starts_with("//")
}

/// POSIX path normalization (oracle `posixpath.normpath`).
fn normpath(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(component),
        }
    }
    parts.join("/")
}

/// POSIX dirname (oracle `posixpath.dirname`).
fn dirname(path: &str) -> &str {
    match path.rfind('/') {
        Some(pos) => &path[..pos],
        None => "",
    }
}

/// Resolve an internal EPUB link against `base_path` (oracle `resolve_epub_href`).
pub fn resolve_epub_href(base_path: &str, raw_href: &str) -> ResolvedEpubHref {
    let raw = raw_href.to_string();
    let (path, fragment) = split_fragment(&raw);
    let fragment = unquote(fragment);
    if is_external(path) {
        return ResolvedEpubHref {
            raw_href: raw,
            resource_href: String::new(),
            fragment,
            external: true,
        };
    }
    let decoded_path = unquote(path);
    let mut resource = if let Some(stripped) = decoded_path.strip_prefix('/') {
        normpath(stripped)
    } else if !decoded_path.is_empty() {
        let base_dir = dirname(base_path);
        let joined = if base_dir.is_empty() {
            decoded_path.clone()
        } else {
            format!("{base_dir}/{decoded_path}")
        };
        normpath(&joined)
    } else {
        // Fragment-only href targets the base file itself.
        normpath(base_path)
    };
    if resource == "." {
        resource = String::new();
    }
    ResolvedEpubHref {
        raw_href: raw,
        resource_href: resource,
        fragment,
        external: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_fragment_and_absolute_paths() {
        let r = resolve_epub_href("OEBPS/toc.ncx", "body.xhtml#part-1");
        assert_eq!(r.resource_href, "OEBPS/body.xhtml");
        assert_eq!(r.fragment, "part-1");
        assert_eq!(r.target_key(), "OEBPS/body.xhtml#part-1");
        assert!(!r.external);

        // Fragment-only targets the base file.
        let r = resolve_epub_href("OEBPS/body.xhtml", "#section-1");
        assert_eq!(r.resource_href, "OEBPS/body.xhtml");

        // Absolute path resolves from the zip root.
        let r = resolve_epub_href("OEBPS/toc.ncx", "/OEBPS/other.xhtml");
        assert_eq!(r.resource_href, "OEBPS/other.xhtml");
    }

    #[test]
    fn marks_external_and_preserves_plus() {
        let r = resolve_epub_href("OEBPS/toc.ncx", "https://example.com/x#y");
        assert!(r.external);
        assert!(r.resource_href.is_empty());

        let r = resolve_epub_href("OEBPS/toc.ncx", "my+file.xhtml");
        assert_eq!(r.resource_href, "OEBPS/my+file.xhtml");

        let r = resolve_epub_href("OEBPS/toc.ncx", "my%20file.xhtml#a%20b");
        assert_eq!(r.resource_href, "OEBPS/my file.xhtml");
        assert_eq!(r.fragment, "a b");
    }

    #[test]
    fn matches_oracle_edge_cases() {
        // (base, raw, resource, fragment, external) — dumped from the oracle.
        let cases = [
            (
                "OEBPS/sub/toc.ncx",
                "../body.xhtml",
                "OEBPS/body.xhtml",
                "",
                false,
            ),
            ("OEBPS/toc.ncx", "", "OEBPS/toc.ncx", "", false),
            (
                "OEBPS/toc.ncx",
                "a/./b/../c.xhtml",
                "OEBPS/a/c.xhtml",
                "",
                false,
            ),
            ("content.opf", "ch1.xhtml", "ch1.xhtml", "", false),
        ];
        for (base, raw, resource, fragment, external) in cases {
            let r = resolve_epub_href(base, raw);
            assert_eq!(r.resource_href, resource, "{base} + {raw}");
            assert_eq!(r.fragment, fragment, "{base} + {raw}");
            assert_eq!(r.external, external, "{base} + {raw}");
        }
    }
}
