pub struct TailWindow { max_bytes: usize, text: String, starts_at_boundary: bool }
impl TailWindow {
    pub fn new(max_bytes: usize) -> Self { Self { max_bytes, text: String::new(), starts_at_boundary: true } }
    pub fn append(&mut self, text: &str, bytes: usize, starts_at_line_boundary: bool) {
        if bytes == 0 { return; }
        if bytes >= self.max_bytes { self.text.clear(); self.starts_at_boundary = starts_at_line_boundary; }
        self.text.push_str(text);
        if bytes >= self.max_bytes || self.text.len() > self.max_bytes.saturating_mul(2) { self.trim(); }
    }
    fn trim(&mut self) {
        if self.text.len() <= self.max_bytes { return; }
        let mut start = self.text.len() - self.max_bytes;
        while !self.text.is_char_boundary(start) { start += 1; }
        self.starts_at_boundary = start == 0 || self.text.as_bytes()[start - 1] == b'\n';
        self.text.drain(..start);
    }
    pub fn text(&mut self) -> &str { self.trim(); &self.text }
    pub fn starts_at_line_boundary(&self) -> bool { self.starts_at_boundary }
}
