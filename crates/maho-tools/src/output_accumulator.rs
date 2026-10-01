use std::{io::Write, path::PathBuf};
use crate::{definition::ToolError, tail_window::TailWindow, truncate::*};
pub struct OutputAccumulatorOptions { pub max_lines: usize, pub max_bytes: usize, pub temp_file_prefix: String }
impl Default for OutputAccumulatorOptions {
    fn default() -> Self { Self { max_lines: DEFAULT_MAX_LINES, max_bytes: DEFAULT_MAX_BYTES, temp_file_prefix: "pi-output".into() } }
}
pub struct OutputSnapshot { pub content: String, pub truncation: TruncationResult, pub full_output_path: Option<PathBuf> }
pub struct OutputAccumulator {
    options: OutputAccumulatorOptions, tail: TailWindow, pending_utf8: Vec<u8>, raw: Vec<u8>, total_raw_bytes: usize,
    total_decoded_bytes: usize, completed_lines: usize, current_line_bytes: usize, finished: bool,
    temp: Option<std::fs::File>, temp_path: Option<PathBuf>,
}
impl OutputAccumulator {
    pub fn new(options: OutputAccumulatorOptions) -> Self {
        Self { tail: TailWindow::new(options.max_bytes.saturating_mul(2).max(1)), options, pending_utf8: Vec::new(), raw: Vec::new(), total_raw_bytes: 0,
            total_decoded_bytes: 0, completed_lines: 0, current_line_bytes: 0, finished: false, temp: None, temp_path: None }
    }
    fn total_lines(&self) -> usize { self.completed_lines + usize::from(self.current_line_bytes > 0) }
    fn append_decoded(&mut self, text: &str) {
        if text.is_empty() { return; }
        self.tail.append(text, text.len(), self.current_line_bytes == 0);
        self.total_decoded_bytes += text.len();
        self.completed_lines += text.bytes().filter(|b| *b == b'\n').count();
        if let Some(index) = text.rfind('\n') { self.current_line_bytes = text.len()-index-1; }
        else { self.current_line_bytes += text.len(); }
    }
    fn needs_spill(&self) -> bool { self.total_raw_bytes > self.options.max_bytes || self.total_decoded_bytes > self.options.max_bytes || self.total_lines() > self.options.max_lines }
    fn ensure_temp(&mut self) -> Result<(), ToolError> {
        if self.temp_path.is_some() { return Ok(()); }
        let temp = tempfile::Builder::new().prefix(&format!("{}-", self.options.temp_file_prefix)).suffix(".log").tempfile()?;
        let (mut file, path) = temp.keep().map_err(|e| ToolError::Io(e.error))?;
        if let Err(error) = file.write_all(&self.raw) {
            std::fs::remove_file(&path)?; return Err(error.into());
        }
        self.raw.clear(); self.temp = Some(file); self.temp_path = Some(path); Ok(())
    }
    fn persist(&mut self, data: &[u8]) -> Result<(), ToolError> {
        if self.temp.is_some() || self.needs_spill() { self.ensure_temp()?; }
        if let Some(file) = &mut self.temp { file.write_all(data)?; }
        else { self.raw.extend_from_slice(data); }
        Ok(())
    }
    pub fn append(&mut self, data: &[u8]) -> Result<(), ToolError> {
        if self.finished { return Err(ToolError::Message("Cannot append to a finished output accumulator".into())); }
        self.total_raw_bytes += data.len(); self.pending_utf8.extend_from_slice(data);
        let mut decoded = String::new(); let mut consumed = 0;
        while consumed < self.pending_utf8.len() {
            match std::str::from_utf8(&self.pending_utf8[consumed..]) {
                Ok(text) => { decoded.push_str(text); consumed = self.pending_utf8.len(); },
                Err(error) => {
                    let end = consumed + error.valid_up_to();
                    decoded.push_str(&String::from_utf8_lossy(&self.pending_utf8[consumed..end])); consumed = end;
                    if let Some(length) = error.error_len() { decoded.push('\u{fffd}'); consumed += length; }
                    else { break; }
                }
            }
        }
        self.pending_utf8.drain(..consumed); self.append_decoded(&decoded); self.persist(data)
    }
    pub fn append_text(&mut self, text: &str) -> Result<(), ToolError> {
        if self.finished { return Err(ToolError::Message("Cannot append to a finished output accumulator".into())); }
        self.total_raw_bytes += text.len(); self.append_decoded(text); self.persist(text.as_bytes())
    }
    pub fn finish(&mut self) -> Result<(), ToolError> {
        if self.finished { return Ok(()); } self.finished = true;
        let tail = String::from_utf8_lossy(&self.pending_utf8).into_owned(); self.pending_utf8.clear(); self.append_decoded(&tail);
        if self.needs_spill() { self.ensure_temp()?; } Ok(())
    }
    pub fn snapshot(&mut self, persist_if_truncated: bool) -> Result<OutputSnapshot, ToolError> {
        let text = self.tail.text().to_owned();
        let text = if self.tail.starts_at_line_boundary() { text.as_str() } else { text.find('\n').map_or(text.as_str(), |n| &text[n+1..]) };
        let mut truncation = truncate_tail(text, TruncationOptions { max_lines: self.options.max_lines, max_bytes: self.options.max_bytes });
        truncation.truncated = self.total_lines() > self.options.max_lines || self.total_decoded_bytes > self.options.max_bytes;
        if truncation.truncated && truncation.truncated_by.is_none() { truncation.truncated_by = Some(if self.total_decoded_bytes > self.options.max_bytes { "bytes" } else { "lines" }.into()); }
        truncation.total_lines = self.total_lines(); truncation.total_bytes = self.total_decoded_bytes;
        if persist_if_truncated && truncation.truncated { self.ensure_temp()?; }
        Ok(OutputSnapshot { content: truncation.content.clone(), truncation, full_output_path: self.temp_path.clone() })
    }
    pub fn close_temp_file(&mut self) -> Result<(), ToolError> {
        if let Some(mut file) = self.temp.take() && let Err(error) = file.flush() { self.remove_temp_file()?; return Err(error.into()); } Ok(())
    }
    pub fn remove_temp_file(&mut self) -> Result<(), ToolError> {
        self.temp.take();
        if let Some(path) = &self.temp_path {
            match std::fs::remove_file(path) { Ok(()) => {}, Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}, Err(error) => return Err(error.into()) }
        }
        self.temp_path = None; Ok(())
    }
    pub fn get_last_line_bytes(&self) -> usize { self.current_line_bytes }
}
