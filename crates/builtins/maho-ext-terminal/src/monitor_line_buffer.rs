pub const MONITOR_LINE_BUFFER_MAX_CHARS: usize = 65_536;

#[derive(Default)]
pub struct MonitorLineBuffer {
    tail: Vec<u16>,
}

impl MonitorLineBuffer {
    pub fn append(&mut self, chunk: &str) -> Vec<String> {
        self.tail.extend(chunk.encode_utf16());
        let mut start = 0;
        let mut lines = Vec::new();
        for (index, ch) in self.tail.iter().enumerate() {
            if *ch == u16::from(b'\n') {
                let mut end = index;
                if end > start && self.tail[end - 1] == u16::from(b'\r') { end -= 1; }
                lines.push(String::from_utf16_lossy(&self.tail[start..end]));
                start = index + 1;
            }
        }
        start = start.max(self.tail.len().saturating_sub(MONITOR_LINE_BUFFER_MAX_CHARS));
        self.tail.drain(..start);
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_preserve_tail_and_strip_crlf() {
        let mut buffer = MonitorLineBuffer::default();
        assert!(buffer.append("first\r").is_empty());
        assert_eq!(buffer.append("\nsecond\nthi"),["first","second"]);
        assert_eq!(buffer.append("rd\n"),["third"]);
    }

    #[test]
    fn unterminated_line_retains_last_utf16_units() {
        let mut buffer = MonitorLineBuffer::default();
        assert!(buffer.append(&"x".repeat(MONITOR_LINE_BUFFER_MAX_CHARS + 10)).is_empty());
        assert_eq!(buffer.append("\n")[0].len(),MONITOR_LINE_BUFFER_MAX_CHARS);
        assert!(buffer.append(&"\u{1f600}".repeat(MONITOR_LINE_BUFFER_MAX_CHARS)).is_empty());
        assert_eq!(buffer.append("\n")[0].encode_utf16().count(),MONITOR_LINE_BUFFER_MAX_CHARS);
    }
}
