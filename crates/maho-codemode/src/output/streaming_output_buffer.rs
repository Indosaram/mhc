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
}

impl TailBuffer {
    pub fn new(max_bytes: usize) -> Self { Self { max_bytes, text: String::new() } }
    pub fn append(&mut self, text: &str) {
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
    pub fn text(&self) -> &str { &self.text }
    pub fn bytes(&self) -> usize { self.text.len() }
}
