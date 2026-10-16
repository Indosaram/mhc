use std::{io::{Read, Seek, SeekFrom, Write}, path::PathBuf};
use crate::{definition::ToolError, tail_window::TailWindow, truncate::*};
pub struct OutputAccumulatorOptions { pub max_lines: usize, pub max_bytes: usize, pub temp_file_prefix: String }
impl Default for OutputAccumulatorOptions {
    fn default() -> Self { Self { max_lines: DEFAULT_MAX_LINES, max_bytes: DEFAULT_MAX_BYTES, temp_file_prefix: "pi-output".into() } }
}
pub struct OutputSnapshot { pub content: String, pub truncation: TruncationResult, pub full_output_path: Option<PathBuf> }
pub struct FullOutput { pub content: String, pub truncated: bool }
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
    pub fn read_full_output(&mut self, max_bytes: usize) -> Result<FullOutput, ToolError> {
        let Some(file) = &mut self.temp else {
            return Ok(truncate_full_output(&self.raw, max_bytes));
        };
        file.flush()?;
        let size = file.metadata()?.len();
        if size <= max_bytes as u64 {
            file.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::with_capacity(size as usize);
            file.read_to_end(&mut bytes)?;
            return Ok(truncate_full_output(&bytes, max_bytes));
        }
        let head_len = max_bytes / 2;
        let tail_len = max_bytes - max_bytes / 2;
        let tail_offset = size.saturating_sub(tail_len as u64);
        if head_len == 0 {
            let mut tail = vec![0; tail_len];
            file.seek(SeekFrom::Start(tail_offset))?;
            file.read_exact(&mut tail)?;
            let mut tail_start = 0;
            while tail_start < tail.len() && tail[tail_start] & 0xc0 == 0x80 { tail_start += 1; }
            let omitted = size.saturating_sub((tail.len() - tail_start) as u64);
            return Ok(FullOutput { content: format!("\n\n[... {omitted} bytes omitted ...]\n\n{}", String::from_utf8_lossy(&tail[tail_start..])), truncated: true });
        }
        let mut head = vec![0; head_len];
        let mut head_extra = [0u8; 3];
        let mut tail = vec![0; tail_len];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut head)?;
        let head_extra_len = (size as usize).saturating_sub(head_len).min(head_extra.len());
        if tail_offset < (head_len + head_extra_len) as u64 {
            file.seek(SeekFrom::Start(0))?;
            let mut all = Vec::new();
            file.read_to_end(&mut all)?;
            return Ok(truncate_full_output(&all, max_bytes));
        }
        file.read_exact(&mut head_extra[..head_extra_len])?;
        file.seek(SeekFrom::Start(tail_offset))?;
        file.read_exact(&mut tail)?;
        let mut head_end = head_len;
        let mut sequence_start = head_len - 1;
        while sequence_start > 0 && head[sequence_start] & 0xc0 == 0x80 { sequence_start -= 1; }
        let lead = head[sequence_start];
        let width = if lead < 0x80 { 1 } else if lead < 0xe0 { 2 } else if lead < 0xf0 { 3 } else { 4 };
        let available = head_len - sequence_start + head_extra[..head_extra_len].iter().take_while(|byte| **byte & 0xc0 == 0x80).count();
        if width > available { head_end = sequence_start; }
        let mut tail_start = 0;
        while tail_start < tail.len() && (tail[tail_start] & 0xc0) == 0x80 { tail_start += 1; }
        let omitted = size.saturating_sub(head_end as u64).saturating_sub((tail.len() - tail_start) as u64);
        let content = format!("{}\n\n[... {omitted} bytes omitted ...]\n\n{}", String::from_utf8_lossy(&head[..head_end]), String::from_utf8_lossy(&tail[tail_start..]));
        Ok(FullOutput { content, truncated: true })
    }
}
fn truncate_full_output(bytes: &[u8], max_bytes: usize) -> FullOutput {
    if bytes.len() <= max_bytes { return FullOutput { content: String::from_utf8_lossy(bytes).into_owned(), truncated: false }; }
    let head_len = max_bytes / 2;
    let tail_len = max_bytes - head_len;
    let mut head_end = head_len.min(bytes.len());
    while head_end > 0 && head_end < bytes.len() && (bytes[head_end] & 0xc0) == 0x80 { head_end -= 1; }
    if head_end == head_len && head_end > 0 {
        let lead = bytes[head_end - 1];
        if lead >= 0xc2 {
            let width = if lead <= 0xdf { 2 } else if lead <= 0xef { 3 } else if lead <= 0xf4 { 4 } else { 1 };
            if head_end - 1 + width > head_len { head_end -= 1; }
        }
    }
    let mut tail_start = bytes.len().saturating_sub(tail_len); while tail_start < bytes.len() && (bytes[tail_start] & 0xc0) == 0x80 { tail_start += 1; }
    let omitted = tail_start.saturating_sub(head_end);
    FullOutput { content: format!("{}\n\n[... {omitted} bytes omitted ...]\n\n{}", String::from_utf8_lossy(&bytes[..head_end]), String::from_utf8_lossy(&bytes[tail_start..])), truncated: true }
}
