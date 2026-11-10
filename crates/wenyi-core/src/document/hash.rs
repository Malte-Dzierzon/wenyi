//! Source content hashing. Oracle: `ingest/source_hash.py`.

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::{Error, Result};

/// SHA-256 hex digest of a file's raw bytes (1 MiB streaming chunks).
pub fn source_sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| Error::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(&hasher.finalize()))
}
/// Hex-encode digest bytes (shared with policy fingerprints).
pub fn hex_of(bytes: &[u8]) -> String {
    hex_encode(bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}
