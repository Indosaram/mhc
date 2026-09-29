//! Port of senpi packages/ai/src/utils/sanitize-unicode.ts.
//!
//! Rust `str` cannot hold unpaired surrogates, so the TS sanitizer operates on UTF-16 input here.

/// Removes unpaired UTF-16 surrogates and decodes the rest.
pub fn sanitize_surrogates_utf16(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied()).filter_map(Result::ok).collect()
}

/// A `str` is already surrogate-free; returned unchanged for call-site parity.
pub fn sanitize_surrogates(text: &str) -> String {
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_unpaired_keeps_pairs() {
        let mut units: Vec<u16> = "Hello 🙈 World".encode_utf16().collect();
        assert_eq!(sanitize_surrogates_utf16(&units), "Hello 🙈 World");
        units = "Text ".encode_utf16().chain([0xD83D]).chain(" here".encode_utf16()).collect();
        assert_eq!(sanitize_surrogates_utf16(&units), "Text  here");
        units = vec![0xDC00, 0x61];
        assert_eq!(sanitize_surrogates_utf16(&units), "a");
        assert_eq!(sanitize_surrogates("ok"), "ok");
    }
}
