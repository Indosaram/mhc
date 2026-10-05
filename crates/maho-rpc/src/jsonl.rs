//! Strict LF framing, ported from modes/rpc/jsonl.ts.
use serde::Serialize;

pub const MAX_RPC_LINE_CHARACTERS: usize = 16 * 1024 * 1024;

pub fn serialize_json_line(value: &impl Serialize) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    Ok(line)
}

#[derive(Debug, PartialEq, Eq)]
pub enum LineRecord {
    Line(String),
    Oversized,
}

#[derive(Debug, thiserror::Error)]
#[error("maxLineLength must be greater than zero.")]
pub struct InvalidLineLimit;

/// Incremental UTF-8 decoder and LF reader. Limits count UTF-16 code units,
/// matching JavaScript String.length, not UTF-8 bytes or Unicode scalars.
pub struct JsonlLineReader {
    max_line_length: usize,
    buffer: String,
    characters: usize,
    undecoded: Vec<u8>,
    discarding: bool,
}

impl Default for JsonlLineReader {
    fn default() -> Self {
        Self { max_line_length: usize::MAX, buffer: String::new(), characters: 0,
            undecoded: Vec::new(), discarding: false }
    }
}

impl JsonlLineReader {
    pub fn new(max_line_length: usize) -> Result<Self, InvalidLineLimit> {
        if max_line_length == 0 { return Err(InvalidLineLimit); }
        Ok(Self { max_line_length, ..Self::default() })
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<LineRecord> {
        self.undecoded.extend_from_slice(chunk);
        let mut records = Vec::new();
        loop {
            match std::str::from_utf8(&self.undecoded) {
                Ok(text) => {
                    let text = text.to_owned();
                    self.undecoded.clear();
                    self.consume(&text, &mut records);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    let text = String::from_utf8_lossy(&self.undecoded[..valid]).into_owned();
                    let invalid = error.error_len();
                    self.undecoded.drain(..valid.saturating_add(invalid.unwrap_or(0)));
                    self.consume(&text, &mut records);
                    match invalid {
                        Some(_) => self.consume("\u{fffd}", &mut records),
                        None => break,
                    }
                }
            }
        }
        records
    }

    pub fn finish(&mut self) -> Vec<LineRecord> {
        let mut records = Vec::new();
        if !self.undecoded.is_empty() {
            let text = String::from_utf8_lossy(&self.undecoded).into_owned();
            self.undecoded.clear();
            self.consume(&text, &mut records);
        }
        if !self.discarding && !self.buffer.is_empty() {
            self.emit(&mut records);
        }
        self.buffer.clear();
        self.characters = 0;
        self.discarding = false;
        records
    }

    fn emit(&mut self, records: &mut Vec<LineRecord>) {
        if self.buffer.ends_with('\r') { self.buffer.pop(); }
        records.push(LineRecord::Line(std::mem::take(&mut self.buffer)));
        self.characters = 0;
    }

    fn consume(&mut self, text: &str, records: &mut Vec<LineRecord>) {
        for ch in text.chars() {
            if ch == '\n' {
                if self.discarding { self.discarding = false; }
                else { self.emit(records); }
            } else if !self.discarding {
                self.characters = self.characters.saturating_add(ch.len_utf16());
                if self.characters > self.max_line_length {
                    self.buffer.clear();
                    self.characters = 0;
                    self.discarding = true;
                    records.push(LineRecord::Oversized);
                } else { self.buffer.push(ch); }
            }
        }
    }
}

/// Handle for the reader task `attach_jsonl_line_reader` spawns. Dropping it stops reading.
pub struct JsonlLineReaderTask { task: tokio::task::JoinHandle<()> }

impl JsonlLineReaderTask {
    /// Stop the reader without waiting for the stream to end (senpi's returned detach function).
    pub fn detach(self) { self.task.abort(); }
}

/// Attach an LF-only JSONL reader to an async stream, mirroring senpi's `attachJsonlLineReader`.
///
/// `on_line` runs for every framed record as soon as its `\n` arrives (a trailing `\r` is dropped);
/// `on_oversized` fires once when a record exceeds `max_line_length` UTF-16 code units and its
/// remainder is discarded up to the next `\n`. A zero limit is rejected, exactly like the TS guard.
pub fn attach_jsonl_line_reader<R, F, O>(
    stream: R,
    mut on_line: F,
    max_line_length: usize,
    mut on_oversized: O,
) -> Result<JsonlLineReaderTask, InvalidLineLimit>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    F: FnMut(String) + Send + 'static,
    O: FnMut() + Send + 'static,
{
    let mut reader = JsonlLineReader::new(max_line_length)?;
    let task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut stream = stream;
        let mut chunk = [0u8; 8192];
        while let Ok(count) = stream.read(&mut chunk).await {
            if count == 0 { break; }
            for record in reader.push(&chunk[..count]) {
                match record { LineRecord::Line(line) => on_line(line), LineRecord::Oversized => on_oversized() }
            }
        }
        for record in reader.finish() {
            match record { LineRecord::Line(line) => on_line(line), LineRecord::Oversized => on_oversized() }
        }
    });
    Ok(JsonlLineReaderTask { task })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn attached_reader_frames_lf_lines_and_discards_an_oversized_record() {
        use tokio::io::AsyncWriteExt;
        let (mut client, host) = tokio::io::duplex(64);
        let (lines_tx, mut lines_rx) = tokio::sync::mpsc::unbounded_channel();
        let (oversized_tx, mut oversized_rx) = tokio::sync::mpsc::unbounded_channel();
        let _handle = attach_jsonl_line_reader(host, move |line| { let _ = lines_tx.send(line); }, 4, move || { let _ = oversized_tx.send(()); }).unwrap();
        client.write_all(b"ab\ncd\r\nefghij\nkl\n").await.unwrap();
        client.shutdown().await.unwrap();
        drop(client);
        let mut lines = Vec::new();
        while let Some(line) = lines_rx.recv().await { lines.push(line); }
        assert_eq!(lines, vec!["ab".to_string(), "cd".into(), "kl".into()]);
        assert_eq!(oversized_rx.try_recv(), Ok(()));
        assert!(oversized_rx.try_recv().is_err());
    }
    #[tokio::test]
    async fn attached_reader_flushes_an_unterminated_record_at_eof() {
        use tokio::io::AsyncWriteExt;
        let (mut client, host) = tokio::io::duplex(64);
        let (lines_tx, mut lines_rx) = tokio::sync::mpsc::unbounded_channel();
        let _handle = attach_jsonl_line_reader(host, move |line| { let _ = lines_tx.send(line); }, MAX_RPC_LINE_CHARACTERS, || {}).unwrap();
        client.write_all(b"{\r").await.unwrap();
        drop(client);
        assert_eq!(lines_rx.recv().await, Some("{".into()));
        assert_eq!(lines_rx.recv().await, None);
    }
    #[test]
    fn attached_reader_rejects_a_zero_limit() {
        let (client, host) = tokio::io::duplex(8);
        assert!(attach_jsonl_line_reader(host, |_| {}, 0, || {}).is_err());
        drop(client);
    }
    #[test]
    fn serializes_unicode_separators() {
        let line = serialize_json_line(&json!({"text":"a\u{2028}b\u{2029}c"})).unwrap();
        assert_eq!(line, "{\"text\":\"a\u{2028}b\u{2029}c\"}\n");
    }
    #[test]
    fn splits_only_lf() {
        let mut reader = JsonlLineReader::default();
        assert_eq!(reader.push("a\u{2028}b\u{2029}c\n".as_bytes()), vec![LineRecord::Line("a\u{2028}b\u{2029}c".into())]);
    }
    #[test]
    fn handles_crlf() {
        let mut reader = JsonlLineReader::default();
        assert_eq!(reader.push(b"a\r\nb\r\n"), vec![LineRecord::Line("a".into()),LineRecord::Line("b".into())]);
    }
    #[test]
    fn emits_unterminated_record_at_eof() {
        let mut reader = JsonlLineReader::default();
        assert!(reader.push(b"{}\r").is_empty());
        assert_eq!(reader.finish(),vec![LineRecord::Line("{}".into())]);
    }
    #[test]
    fn resynchronizes_once_after_oversized_record() {
        let mut reader = JsonlLineReader::new(8).unwrap();
        assert_eq!(reader.push(b"123456789"),vec![LineRecord::Oversized]);
        assert_eq!(reader.push(b"discarded\n{}\n"),vec![LineRecord::Line("{}".into())]);
    }
    #[test]
    fn accepts_fully_escaped_maximum_message() {
        let line = serialize_json_line(&json!({"type":"prompt","message":"\0".repeat(1_000_000)})).unwrap();
        let mut reader = JsonlLineReader::new(MAX_RPC_LINE_CHARACTERS).unwrap();
        assert_eq!(reader.push(line.as_bytes()),vec![LineRecord::Line(line.trim_end_matches('\n').into())]);
    }
    #[test]
    fn split_utf8_is_decoded_before_counting_utf16() {
        let mut reader = JsonlLineReader::new(2).unwrap();
        let bytes = "😀\n".as_bytes();
        assert!(reader.push(&bytes[..2]).is_empty());
        assert_eq!(reader.push(&bytes[2..]),vec![LineRecord::Line("😀".into())]);
    }
    #[test]
    fn unfinished_utf8_becomes_replacement_at_eof() {
        let mut reader = JsonlLineReader::default();
        assert!(reader.push(&[0xf0,0x9f]).is_empty());
        assert_eq!(reader.finish(),vec![LineRecord::Line("\u{fffd}".into())]);
    }
    #[test]
    fn utf16_limit_rejects_astral_scalar_and_recovers() {
        let mut reader = JsonlLineReader::new(1).unwrap();
        assert_eq!(reader.push("😀\na\n".as_bytes()),vec![LineRecord::Oversized,LineRecord::Line("a".into())]);
    }
}
