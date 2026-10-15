#[derive(Debug, PartialEq, Eq)]
pub struct ByteSlice<'a> {
    pub text: &'a str,
    pub bytes: usize,
}

pub fn truncate_head_bytes(text: &str, max_bytes: usize) -> ByteSlice<'_> {
    let mut end = max_bytes.min(text.len());
    while !text.is_char_boundary(end) { end -= 1; }
    ByteSlice { text: &text[..end], bytes: end }
}

pub fn truncate_tail_bytes(text: &str, max_bytes: usize) -> ByteSlice<'_> {
    let mut start = text.len().saturating_sub(max_bytes);
    while !text.is_char_boundary(start) { start += 1; }
    ByteSlice { text: &text[start..], bytes: text.len() - start }
}

pub struct TailBuffer {
    max_bytes: usize,
    text: String,
    /// Visible window is `text[start..]`; the prefix is compacted lazily so a
    /// full buffer does not memmove its whole contents on every append.
    start: usize,
}

impl TailBuffer {
    pub fn new(max_bytes: usize) -> Self { Self { max_bytes, text: String::new(), start: 0 } }
    pub fn append(&mut self, text: &str) {
        if text.is_empty() { return; }
        if self.max_bytes == 0 { self.text.clear(); self.start = 0; return; }
        if text.len() >= self.max_bytes {
            self.text.clear();
            self.text.push_str(truncate_tail_bytes(text, self.max_bytes).text);
            self.start = 0;
        } else {
            self.text.push_str(text);
            let mut start = self.text.len().saturating_sub(self.max_bytes).max(self.start);
            while !self.text.is_char_boundary(start) { start += 1; }
            if start >= self.max_bytes {
                self.text.drain(..start);
                self.start = 0;
            } else {
                self.start = start;
            }
        }
    }
    pub fn text(&self) -> &str { &self.text[self.start..] }
    pub fn bytes(&self) -> usize { self.text.len() - self.start }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DrainTailBuffer { max_bytes: usize, text: String }

    impl DrainTailBuffer {
        fn append(&mut self, text: &str) {
            if text.is_empty() { return; }
            if self.max_bytes == 0 { self.text.clear(); return; }
            if text.len() >= self.max_bytes {
                self.text = truncate_tail_bytes(text, self.max_bytes).text.into();
            } else {
                self.text.push_str(text);
                let mut start = self.text.len().saturating_sub(self.max_bytes);
                while !self.text.is_char_boundary(start) { start += 1; }
                self.text.drain(..start);
            }
        }
    }

    #[test]
    fn tail_buffer_matches_drain_behaviour_over_many_appends() {
        let pieces = ["a", "line\n", "é", "日本語", "🦀x", "", "0123456789abcdef", "ü🦀", "z\n"];
        for max_bytes in [0usize, 1, 2, 3, 4, 5, 7, 16, 33, 100] {
            let mut buffer = TailBuffer::new(max_bytes);
            let mut reference = DrainTailBuffer { max_bytes, text: String::new() };
            for i in 0..2_000usize {
                let piece = if i % 97 == 0 {
                    "日".repeat(max_bytes / 2 + 1)
                } else {
                    pieces[(i * 7 + i / 3) % pieces.len()].to_owned()
                };
                buffer.append(&piece);
                reference.append(&piece);
                assert_eq!(buffer.text(), reference.text, "max_bytes={max_bytes} step={i}");
                assert_eq!(buffer.bytes(), reference.text.len(), "max_bytes={max_bytes} step={i}");
                assert!(buffer.text.len() < 2 * max_bytes.max(1) + piece.len());
            }
        }
    }
}
