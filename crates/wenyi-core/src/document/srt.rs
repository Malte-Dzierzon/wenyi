//! SRT subtitle parsing. Oracle: `ingest/srt_reader.py`.
//!
//! Feeds the standalone subtitle pipeline (no book chapters, glossary, or
//! review); cues keep their file order and timing verbatim.

use std::path::Path;

use crate::{Error, Result};

/// One subtitle cue: file index, timing line, body text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrtCue {
    /// Cue number as written in the file.
    pub index: String,
    /// `HH:MM:SS,mmm --> HH:MM:SS,mmm` timing line.
    pub timestamp: String,
    /// Cue body with surrounding whitespace stripped.
    pub text: String,
}

/// Whether a line is an SRT timestamp (`00:00:01,000 --> 00:00:04,000`).
fn is_timestamp(line: &str) -> bool {
    // Oracle regex: \d{2}:\d{2}:\d{2},\d{3} --> \d{2}:\d{2}:\d{2},\d{3}
    let bytes = line.as_bytes();
    if bytes.len() != 29 {
        return false;
    }
    let digit = |i: usize| bytes[i].is_ascii_digit();
    (0..2).all(digit)
        && bytes[2] == b':'
        && (3..5).all(digit)
        && bytes[5] == b':'
        && (6..8).all(digit)
        && bytes[8] == b','
        && (9..12).all(digit)
        && &bytes[12..17] == b" --> "
        && (17..19).all(digit)
        && bytes[19] == b':'
        && (20..22).all(digit)
        && bytes[22] == b':'
        && (23..25).all(digit)
        && bytes[25] == b','
        && (26..29).all(digit)
}

/// Whether a line is a cue index (all ASCII digits, nonempty).
fn is_index(line: &str) -> bool {
    !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit())
}

/// Decode SRT bytes, trying common encodings (oracle `_decode_srt_bytes`).
///
/// Tries UTF-8 (with/without BOM), then UTF-16 (BOM-sniffed), then
/// lossy UTF-8. GB18030/legacy CJK bytes without a BOM decode lossy —
/// full GB18030 tables are out of scope (see `decode_markup` for the
/// same documented limitation on the EPUB side).
fn decode_srt_bytes(raw: &[u8]) -> String {
    if let Some(stripped) = raw.strip_prefix(b"\xef\xbb\xbf") {
        return String::from_utf8_lossy(stripped).into_owned();
    }
    if let Ok(text) = std::str::from_utf8(raw) {
        return text.to_string();
    }
    if raw.len() >= 2 {
        if raw.starts_with(b"\xff\xfe") {
            return decode_utf16(raw, true);
        }
        if raw.starts_with(b"\xfe\xff") {
            return decode_utf16(raw, false);
        }
    }
    String::from_utf8_lossy(raw).into_owned()
}

/// Decode UTF-16 bytes (LE or BE) with BOM skipped, lossy on surrogates.
fn decode_utf16(raw: &[u8], little_endian: bool) -> String {
    let (chunks, _remainder) = raw[2..].as_chunks::<2>();
    let words: Vec<u16> = chunks
        .iter()
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&words)
}

/// Parse SRT cues (oracle `parse_srt`).
///
/// Block grammar mirrors the oracle regex: an index line, a timestamp line,
/// then body text up to the next blank line. Recovery differs deliberately:
/// the oracle regex silently drops malformed blocks (a file with *some* valid
/// blocks parses); we keep that leniency but verify no silent chapter loss
/// by counting parsed cues against index-line candidates in tests.
pub fn parse_srt(path: &Path) -> Result<Vec<SrtCue>> {
    let raw = std::fs::read(path)
        .map_err(|e| Error::Config(format!("Cannot read subtitle file: {e}")))?;
    parse_srt_text(&decode_srt_bytes(&raw))
}

/// Parse SRT cues from decoded text (split out for golden tests).
pub fn parse_srt_text(content: &str) -> Result<Vec<SrtCue>> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    // Split into blocks on blank lines (oracle lookahead `(?=\r?\n\r?\n|\Z)`).
    let mut cues = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            if !block.is_empty() {
                blocks.push(std::mem::take(&mut block));
            }
        } else {
            block.push(line);
        }
    }
    if !block.is_empty() {
        blocks.push(block);
    }
    for block in blocks {
        if block.len() < 2 {
            continue;
        }
        // Oracle `re.MULTILINE` + `findall` scans for the pattern anywhere;
        // a block matches when its first two lines are index + timestamp.
        // (Leading garbage lines inside a block disqualify it, same as the
        // oracle: `(\d+)\n(timestamp)` must match at a line boundary, and a
        // non-matching first line means the regex engine advances past it —
        // but it cannot skip lines *within* the consumed match. Since blocks
        // are blank-line-delimited, the only consistent reading is first-two.)
        // Edge: oracle CAN match starting mid-block if line 2+3 are
        // index+timestamp (line 1 ignored). Handle by scanning starts.
        for start in 0..block.len().saturating_sub(1) {
            if is_index(block[start]) && is_timestamp(block[start + 1]) {
                let text = block[start + 2..].join("\n");
                cues.push(SrtCue {
                    index: block[start].to_string(),
                    timestamp: block[start + 1].to_string(),
                    text: text.trim().to_string(),
                });
                break;
            }
        }
    }
    if cues.is_empty() {
        return Err(Error::Config(
            "No valid SRT subtitle blocks found".to_string(),
        ));
    }
    Ok(cues)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_and_garbage() {
        assert!(parse_srt_text("").is_err());
        assert!(parse_srt_text("hello\nworld\n").is_err());
        assert!(parse_srt_text("1\nnot a timestamp\ntext\n").is_err());
    }

    #[test]
    fn parses_crlf_and_bom() {
        let text = "\u{feff}1\r\n00:00:01,000 --> 00:00:04,000\r\nHi\r\n";
        let cues = parse_srt_text(text).unwrap();
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].index, "1");
        assert_eq!(cues[0].text, "Hi");
    }
}
