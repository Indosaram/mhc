//! File-for-file port of Pico3's bounded byte collector.
use std::borrow::Cow;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Retain { Head, Tail }

#[derive(Clone, Debug)]
pub struct Bounded {
    bytes: Vec<u8>,
    max_bytes: usize,
    max_lines: usize,
    retain: Retain,
    pub dropped_bytes: usize,
    pub dropped_lines: usize,
    pub total: usize,
}
impl Bounded {
    pub const fn new(max_bytes: usize, max_lines: usize, retain: Retain) -> Self {
        Self { bytes: Vec::new(), max_bytes, max_lines, retain,
            dropped_bytes: 0, dropped_lines: 0, total: 0 }
    }
    pub fn push(&mut self, chunk: &[u8]) {
        self.total = self.total.saturating_add(chunk.len());
        if chunk.is_empty() { return; }
        if self.max_bytes == 0 || self.max_lines == 0 { self.drop_bytes(chunk); return; }
        match self.retain {
            Retain::Head => self.push_head(chunk),
            Retain::Tail => self.push_tail(chunk),
        }
    }
    fn push_head(&mut self, chunk: &[u8]) {
        let remaining_bytes = self.max_bytes.saturating_sub(self.bytes.len());
        let remaining_lines = self.max_lines.saturating_sub(count_newlines(&self.bytes));
        if remaining_bytes == 0 || remaining_lines == 0 { self.drop_bytes(chunk); return; }
        let mut take = chunk.len().min(remaining_bytes);
        let mut lines = 0;
        for (index, byte) in chunk.iter().take(take).enumerate() {
            if *byte == b'\n' {
                lines += 1;
                if lines == remaining_lines { take = index + 1; break; }
            }
        }
        self.bytes.extend_from_slice(&chunk[..take]);
        self.drop_bytes(&chunk[take..]);
    }
    fn push_tail(&mut self, chunk: &[u8]) {
        let incoming_start = tail_start(chunk, self.max_bytes, self.max_lines);
        self.drop_bytes(&chunk[..incoming_start]);
        self.bytes.extend_from_slice(&chunk[incoming_start..]);
        let start = tail_start(&self.bytes, self.max_bytes, self.max_lines);
        self.dropped_bytes = self.dropped_bytes.saturating_add(start);
        self.dropped_lines = self.dropped_lines.saturating_add(count_newlines(&self.bytes[..start]));
        self.bytes.drain(..start);
    }
    fn drop_bytes(&mut self, bytes: &[u8]) {
        self.dropped_bytes = self.dropped_bytes.saturating_add(bytes.len());
        self.dropped_lines = self.dropped_lines.saturating_add(count_newlines(bytes));
    }
    pub const fn dropped(&self) -> usize { self.dropped_bytes }
    /// Like TextDecoder, decode only after retention; incomplete UTF-8 becomes U+FFFD.
    pub fn text(&self) -> Cow<'_, str> {
        let bytes = self.bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&self.bytes);
        String::from_utf8_lossy(bytes)
    }
    pub fn bytes(&self) -> &[u8] { &self.bytes }
}
fn tail_start(bytes: &[u8], max_bytes: usize, max_lines: usize) -> usize {
    let mut start = bytes.len().saturating_sub(max_bytes);
    let mut excess = count_newlines(&bytes[start..]).saturating_sub(max_lines);
    while start < bytes.len() && excess > 0 {
        if bytes[start] == b'\n' { excess -= 1; }
        start += 1;
    }
    start
}
fn count_newlines(bytes: &[u8]) -> usize { bytes.iter().filter(|b| **b == b'\n').count() }

#[cfg(test)]
#[path = "bounded_tests.rs"]
mod tests;
