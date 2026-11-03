//! Incremental UTF-8 decoding for byte streams (no senpi counterpart: Node's `TextDecoder` in
//! stream mode does this natively).

/// Decodes UTF-8 across chunk boundaries: a multibyte character split between two chunks is
/// kept intact instead of becoming two U+FFFD. Genuinely invalid bytes are replaced exactly like
/// [`String::from_utf8_lossy`].
#[derive(Debug, Default)]
pub struct Utf8ChunkDecoder {
    /// Incomplete trailing sequence of the previous chunk (at most 3 bytes).
    pending: Vec<u8>,
}

impl Utf8ChunkDecoder {
    /// Appends the decoded text of `chunk` to `out`, holding back an incomplete trailing sequence.
    pub fn push(&mut self, chunk: &[u8], out: &mut String) {
        if self.pending.is_empty() {
            self.decode(chunk, out);
            return;
        }
        let mut joined = std::mem::take(&mut self.pending);
        joined.extend_from_slice(chunk);
        self.decode(&joined, out);
    }

    /// Flushes a held-back incomplete sequence at end of stream as U+FFFD.
    pub fn finish(&mut self, out: &mut String) {
        if !self.pending.is_empty() {
            out.push_str(&String::from_utf8_lossy(&self.pending));
            self.pending.clear();
        }
    }

    fn decode(&mut self, bytes: &[u8], out: &mut String) {
        let mut chunks = bytes.utf8_chunks().peekable();
        while let Some(chunk) = chunks.next() {
            out.push_str(chunk.valid());
            let invalid = chunk.invalid();
            if invalid.is_empty() {
                continue;
            }
            let is_incomplete_tail = chunks.peek().is_none()
                && matches!(std::str::from_utf8(invalid), Err(error) if error.error_len().is_none());
            if is_incomplete_tail {
                self.pending.extend_from_slice(invalid);
            } else {
                out.push(char::REPLACEMENT_CHARACTER);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Utf8ChunkDecoder;

    fn decode_split(bytes: &[u8], at: usize) -> String {
        let mut decoder = Utf8ChunkDecoder::default();
        let mut out = String::new();
        decoder.push(&bytes[..at], &mut out);
        decoder.push(&bytes[at..], &mut out);
        decoder.finish(&mut out);
        out
    }

    #[test]
    fn keeps_two_three_and_four_byte_chars_split_at_every_position() {
        // Given: text with 2-, 3- and 4-byte characters
        let text = "a\u{e9}b\u{d55c}\u{ae00}c\u{1f600}d";
        for at in 0..=text.len() {
            // When: the bytes arrive as two chunks split at `at`
            let decoded = decode_split(text.as_bytes(), at);
            // Then: the text is exact
            assert_eq!(decoded, text, "split at {at}");
        }
    }

    #[test]
    fn keeps_a_char_split_across_three_chunks() {
        // Given: a 4-byte char delivered one byte at a time
        let mut decoder = Utf8ChunkDecoder::default();
        let mut out = String::new();
        // When
        for byte in "\u{1f600}".as_bytes() {
            decoder.push(&[*byte], &mut out);
        }
        decoder.finish(&mut out);
        // Then
        assert_eq!(out, "\u{1f600}");
    }

    #[test]
    fn replaces_invalid_bytes_like_lossy_decoding_at_every_split() {
        // Given: invalid bytes, a truncated sequence followed by ASCII, and valid text
        let bytes: &[u8] = b"a\xffb\xe2\x82c\xf0\x9f\x98\xe2\x82\xac\xc0\xaf\xed\xa0\x80z";
        let expected = String::from_utf8_lossy(bytes).into_owned();
        for at in 0..=bytes.len() {
            // When / Then
            assert_eq!(decode_split(bytes, at), expected, "split at {at}");
        }
    }

    #[test]
    fn replaces_invalid_byte_with_replacement_char() {
        // Given / When
        let mut decoder = Utf8ChunkDecoder::default();
        let mut out = String::new();
        decoder.push(b"x\xffy", &mut out);
        // Then
        assert_eq!(out, "x\u{fffd}y");
    }

    #[test]
    fn flushes_truncated_tail_as_replacement_char_on_finish() {
        // Given: a stream that ends inside a 3-byte character
        let mut decoder = Utf8ChunkDecoder::default();
        let mut out = String::new();
        decoder.push(&"ok\u{d55c}".as_bytes()[..4], &mut out);
        assert_eq!(out, "ok");
        // When
        decoder.finish(&mut out);
        // Then
        assert_eq!(out, "ok\u{fffd}");
    }
}
