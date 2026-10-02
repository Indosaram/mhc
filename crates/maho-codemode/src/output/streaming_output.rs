use super::{output_meta::format_middle_elision_marker, streaming_output_buffer::{TailBuffer, truncate_head_bytes}};
pub use crate::host_sdk::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncationOptions, truncate_tail};
use std::{path::PathBuf, sync::Arc};
use tokio::io::AsyncWriteExt;

pub const ARTIFACT_DEFAULT_HEAD_BYTES: usize = 3 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputSummary {
    pub output: String,
    pub truncated: bool,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub elided_bytes: Option<usize>,
    pub elided_lines: Option<usize>,
    pub column_dropped_bytes: Option<usize>,
    pub column_truncated_lines: Option<usize>,
    pub artifact_id: Option<PathBuf>,
}

pub type ChunkCallback = Arc<dyn Fn(&str) + Send + Sync>;

pub struct OutputSinkOptions {
    pub artifact_path: Option<PathBuf>,
    pub spill_threshold: usize,
    pub head_bytes: usize,
    pub max_columns: usize,
    pub on_chunk: Option<ChunkCallback>,
    pub chunk_throttle_ms: u64,
}

impl Default for OutputSinkOptions {
    fn default() -> Self {
        Self { artifact_path: None, spill_threshold: DEFAULT_MAX_BYTES, head_bytes: 0, max_columns: 0, on_chunk: None, chunk_throttle_ms: 0 }
    }
}

fn line_count(text: &str) -> usize {
    if text.is_empty() { 0 } else { text.bytes().filter(|byte| *byte == b'\n').count() + 1 }
}

pub struct OutputSink {
    options: OutputSinkOptions,
    tail: TailBuffer,
    head: String,
    total_newlines: usize,
    total_bytes: usize,
    saw_data: bool,
    truncated: bool,
    current_line_bytes: usize,
    column_capped: bool,
    column_dropped_bytes: usize,
    column_truncated_lines: usize,
    last_chunk_time: u64,
    pending_chunk: String,
    before_spill: String,
    file: Option<tokio::fs::File>,
    summary: Option<OutputSummary>,
}

impl OutputSink {
    pub fn new(options: OutputSinkOptions) -> Self {
        let tail = TailBuffer::new(options.spill_threshold);
        Self { options, tail, head: String::new(), total_newlines: 0, total_bytes: 0, saw_data: false, truncated: false, current_line_bytes: 0, column_capped: false, column_dropped_bytes: 0, column_truncated_lines: 0, last_chunk_time: 0, pending_chunk: String::new(), before_spill: String::new(), file: None, summary: None }
    }

    pub async fn push(&mut self, chunk: &str, now_ms: u64) -> std::io::Result<()> {
        if chunk.is_empty() { return Ok(()); }
        self.emit_preview(chunk, now_ms);
        self.total_bytes += chunk.len();
        self.total_newlines += chunk.bytes().filter(|byte| *byte == b'\n').count();
        self.saw_data = true;
        let dropped_before = self.column_dropped_bytes;
        let retained = if self.options.max_columns > 0 { self.clamp_columns(chunk) } else { chunk.into() };
        self.mirror_raw(chunk, self.column_dropped_bytes > dropped_before).await?;
        self.retain(&retained);
        Ok(())
    }

    fn emit_preview(&mut self, chunk: &str, now_ms: u64) {
        let Some(callback) = &self.options.on_chunk else { return; };
        if now_ms.saturating_sub(self.last_chunk_time) >= self.options.chunk_throttle_ms {
            self.last_chunk_time = now_ms;
            self.pending_chunk.push_str(chunk);
            callback(&self.pending_chunk);
            self.pending_chunk.clear();
        } else {
            self.pending_chunk.push_str(chunk);
        }
    }

    async fn mirror_raw(&mut self, chunk: &str, column_cap_dropped: bool) -> std::io::Result<()> {
        let Some(path) = &self.options.artifact_path else { return Ok(()); };
        if let Some(file) = &mut self.file { return file.write_all(chunk.as_bytes()).await; }
        if !column_cap_dropped && self.total_bytes <= self.options.spill_threshold {
            self.before_spill.push_str(chunk);
            return Ok(());
        }
        if let Some(parent) = path.parent() { tokio::fs::create_dir_all(parent).await?; }
        let mut file = tokio::fs::File::create(path).await?;
        file.write_all(self.before_spill.as_bytes()).await?;
        self.before_spill.clear();
        file.write_all(chunk.as_bytes()).await?;
        self.file = Some(file);
        Ok(())
    }

    fn retain(&mut self, text: &str) {
        let mut tail_text = text;
        if self.head.len() < self.options.head_bytes {
            let head = truncate_head_bytes(text, self.options.head_bytes - self.head.len());
            self.head.push_str(head.text);
            tail_text = &text[head.text.len()..];
        }
        self.tail.append(tail_text);
        if self.total_bytes - self.column_dropped_bytes > self.head.len() + self.tail.bytes() { self.truncated = true; }
    }

    fn clamp_columns(&mut self, chunk: &str) -> String {
        let mut output = String::new();
        for segment in chunk.split_inclusive('\n') {
            let newline = segment.ends_with('\n');
            let segment = segment.strip_suffix('\n').unwrap_or(segment);
            if !segment.is_empty() {
                if self.column_capped {
                    self.column_dropped_bytes += segment.len();
                } else {
                    let kept = truncate_head_bytes(segment, self.options.max_columns.saturating_sub(self.current_line_bytes));
                    output.push_str(kept.text);
                    self.current_line_bytes += kept.bytes;
                    if kept.bytes < segment.len() {
                        output.push('…');
                        self.column_dropped_bytes += segment.len() - kept.bytes;
                        self.column_truncated_lines += 1;
                        self.column_capped = true;
                        self.truncated = true;
                    }
                }
            }
            if newline {
                output.push('\n');
                self.current_line_bytes = 0;
                self.column_capped = false;
            }
        }
        output
    }

    pub async fn dump(&mut self, notice: Option<&str>) -> std::io::Result<OutputSummary> {
        if let Some(summary) = &self.summary { return Ok(summary.clone()); }
        if let Some(callback) = &self.options.on_chunk && !self.pending_chunk.is_empty() {
            callback(&self.pending_chunk);
            self.pending_chunk.clear();
        }
        let artifact_id = if let Some(mut file) = self.file.take() {
            file.flush().await?;
            file.shutdown().await?;
            self.options.artifact_path.clone()
        } else { None };
        let mut tail = self.tail.text().to_owned();
        if line_count(&tail) > DEFAULT_MAX_LINES {
            tail = truncate_tail(&tail, TruncationOptions { max_lines: DEFAULT_MAX_LINES, max_bytes: usize::MAX }).content;
            self.truncated = true;
        }
        let total_lines = if self.saw_data { self.total_newlines + 1 } else { 0 };
        let effective_bytes = self.total_bytes.saturating_sub(self.column_dropped_bytes);
        let mut body = format!("{}{tail}", self.head);
        let mut elided_bytes = None;
        let mut elided_lines = None;
        if !self.head.is_empty() && effective_bytes > self.head.len() + tail.len() {
            let bytes = effective_bytes - self.head.len() - tail.len();
            let lines = total_lines.saturating_sub(line_count(&self.head) + line_count(&tail));
            elided_bytes = Some(bytes);
            elided_lines = Some(lines);
            body = format!("{}{}{}{}{tail}", self.head, if self.head.ends_with('\n') { "" } else { "\n" }, format_middle_elision_marker(lines, bytes), if tail.is_empty() || tail.starts_with('\n') { "" } else { "\n" });
            self.truncated = true;
        }
        let summary = OutputSummary {
            output: notice.map_or_else(|| body.clone(), |notice| format!("[{notice}]\n{body}")),
            truncated: self.truncated, total_lines, total_bytes: self.total_bytes,
            output_lines: line_count(&body), output_bytes: body.len(), elided_bytes, elided_lines,
            column_dropped_bytes: (self.column_dropped_bytes > 0).then_some(self.column_dropped_bytes),
            column_truncated_lines: (self.column_truncated_lines > 0).then_some(self.column_truncated_lines), artifact_id,
        };
        self.summary = Some(summary.clone());
        Ok(summary)
    }
}
