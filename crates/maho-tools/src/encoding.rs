//! Port of `packages/coding-agent/src/core/tools/encoding.ts`.
//! UTF-8 and base64 byte helpers shared across tool cores.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

pub fn utf8_byte_length(text: &str) -> usize {
    text.len()
}

pub fn encode_utf8(text: &str) -> &[u8] {
    text.as_bytes()
}

/// Decodes like `Buffer#toString("utf-8")`: invalid sequences become U+FFFD and a leading BOM is kept.
pub fn decode_utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn concat_bytes(chunks: &[&[u8]]) -> Vec<u8> {
    let total: usize = chunks.iter().map(|c| c.len()).sum();
    let mut joined = Vec::with_capacity(total);
    for chunk in chunks {
        joined.extend_from_slice(chunk);
    }
    joined
}

pub fn encode_base64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Decodes base64url text to UTF-8 like `Buffer.from(text, "base64url")`: ignores padding or dangling final char.
pub fn decode_base64_url_utf8(text: &str) -> Result<String, base64::DecodeError> {
    let trimmed = text.trim_end_matches('=');
    let usable = if trimmed.len() % 4 == 1 {
        &trimmed[..trimmed.len() - 1]
    } else {
        trimmed
    };
    let bytes = URL_SAFE_NO_PAD.decode(usable)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
