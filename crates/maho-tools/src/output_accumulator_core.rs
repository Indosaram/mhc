//! Port of `packages/coding-agent/src/core/tools/output-accumulator-core.ts`.

use crate::definition::{ToolError, ToolFuture};
use crate::tail_window::TailWindow;
use crate::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncationOptions, TruncationResult, truncate_tail,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct CoreOutputSnapshot {
    pub content: String,
    pub truncation: TruncationResult,
    pub full_output_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreFullOutput {
    pub content: String,
    pub truncated: bool,
}

/// Where an accumulator keeps the full output once it outgrows the display limits.
pub trait OutputSpill: Send + Sync {
    /// The path truncation notices show the model.
    fn path(&self) -> &Path;
    /// Appends a chunk; a failure is latched and reported by `close`.
    fn write(&mut self, chunk: &[u8]);
    /// Settles every write; rejects with the first write or close failure.
    fn close(&mut self) -> ToolFuture<'_, ()>;
    /// Discards the spill; a rejection leaves it in place for a retry.
    fn remove(&mut self) -> ToolFuture<'_, ()>;
    /// The whole output after `close`, cut by `join_head_and_tail` past `max_bytes`.
    fn read_full(&mut self, max_bytes: usize) -> ToolFuture<'_, CoreFullOutput>;
}

pub type OutputSpillFactory =
    Arc<dyn Fn(&str) -> Result<Box<dyn OutputSpill>, ToolError> + Send + Sync>;

#[derive(Clone, Debug)]
pub struct CoreOutputAccumulatorOptions {
    pub max_lines: usize,
    pub max_bytes: usize,
    pub temp_file_prefix: String,
}

impl Default for CoreOutputAccumulatorOptions {
    fn default() -> Self {
        Self {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
            temp_file_prefix: "pi-output".to_owned(),
        }
    }
}

/// Decode UTF-8 like `TextDecoder.decode(bytes, { stream: true })`: holds back an incomplete
/// trailing multi-byte sequence instead of replacing it with U+FFFD.
fn decode_utf8_streaming_head(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut offset = 0;
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(valid) => {
                out.push_str(valid);
                break;
            }
            Err(err) => {
                let valid_up_to = offset + err.valid_up_to();
                if let Ok(valid) = std::str::from_utf8(&bytes[offset..valid_up_to]) {
                    out.push_str(valid);
                }
                match err.error_len() {
                    Some(bad_len) => {
                        out.push('\u{fffd}');
                        offset = valid_up_to + bad_len;
                    }
                    None => {
                        // Incomplete trailing sequence held back across omission cut.
                        break;
                    }
                }
            }
        }
    }
    out
}

/// Join head and tail bytes around an omission marker, cutting cleanly at UTF-8 boundaries.
pub fn join_head_and_tail(head: &[u8], tail: &[u8], omitted_bytes: usize) -> String {
    let head_text = decode_utf8_streaming_head(head);
    let mut tail_start = 0;
    while tail_start < tail.len() && (tail[tail_start] & 0xc0) == 0x80 {
        tail_start += 1;
    }
    let tail_text = String::from_utf8_lossy(&tail[tail_start..]);
    format!("{head_text}\n\n[... {omitted_bytes} bytes omitted ...]\n\n{tail_text}")
}

/// Incrementally tracks streaming output with bounded memory over an injectable [`OutputSpill`].
pub struct CoreOutputAccumulator {
    max_lines: usize,
    max_bytes: usize,
    temp_file_prefix: String,
    create_spill: OutputSpillFactory,
    pending_utf8: Vec<u8>,
    tail: TailWindow,
    raw_chunks: Vec<Vec<u8>>,
    total_raw_bytes: usize,
    total_decoded_bytes: usize,
    completed_lines: usize,
    total_lines: usize,
    current_line_bytes: usize,
    finished: bool,
    spill: Option<Box<dyn OutputSpill>>,
    spill_open: bool,
}

impl CoreOutputAccumulator {
    pub fn new(options: CoreOutputAccumulatorOptions, spill: OutputSpillFactory) -> Self {
        let max_bytes = options.max_bytes;
        Self {
            max_lines: options.max_lines,
            max_bytes,
            temp_file_prefix: options.temp_file_prefix,
            create_spill: spill,
            pending_utf8: Vec::new(),
            tail: TailWindow::new(max_bytes.saturating_mul(2).max(1)),
            raw_chunks: Vec::new(),
            total_raw_bytes: 0,
            total_decoded_bytes: 0,
            completed_lines: 0,
            total_lines: 0,
            current_line_bytes: 0,
            finished: false,
            spill: None,
            spill_open: false,
        }
    }

    pub fn append(&mut self, data: &[u8]) -> Result<(), ToolError> {
        if self.finished {
            return Err(ToolError::Message(
                "Cannot append to a finished output accumulator".into(),
            ));
        }
        self.total_raw_bytes += data.len();
        self.pending_utf8.extend_from_slice(data);
        let mut decoded = String::new();
        let mut consumed = 0;
        while consumed < self.pending_utf8.len() {
            match std::str::from_utf8(&self.pending_utf8[consumed..]) {
                Ok(text) => {
                    decoded.push_str(text);
                    consumed = self.pending_utf8.len();
                }
                Err(err) => {
                    let end = consumed + err.valid_up_to();
                    decoded.push_str(&String::from_utf8_lossy(&self.pending_utf8[consumed..end]));
                    consumed = end;
                    if let Some(length) = err.error_len() {
                        decoded.push('\u{fffd}');
                        consumed += length;
                    } else {
                        break;
                    }
                }
            }
        }
        self.pending_utf8.drain(..consumed);
        self.append_decoded_text(&decoded);

        if self.spill_open || self.should_use_temp_file() {
            self.ensure_temp_file()?;
            if let Some(spill) = &mut self.spill
                && self.spill_open
            {
                spill.write(data);
            }
        } else if !data.is_empty() {
            self.raw_chunks.push(data.to_vec());
        }
        Ok(())
    }

    pub fn append_text(&mut self, text: &str) -> Result<(), ToolError> {
        if self.finished {
            return Err(ToolError::Message(
                "Cannot append to a finished output accumulator".into(),
            ));
        }
        if text.is_empty() {
            return Ok(());
        }
        self.total_raw_bytes += text.len();
        self.append_decoded_text(text);

        if self.spill_open || self.should_use_temp_file() {
            self.ensure_temp_file()?;
            if let Some(spill) = &mut self.spill
                && self.spill_open
            {
                spill.write(text.as_bytes());
            }
        } else {
            self.raw_chunks.push(text.as_bytes().to_vec());
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), ToolError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let tail = String::from_utf8_lossy(&self.pending_utf8).into_owned();
        self.pending_utf8.clear();
        self.append_decoded_text(&tail);
        if self.should_use_temp_file() {
            self.ensure_temp_file()?;
        }
        Ok(())
    }

    pub fn snapshot(
        &mut self,
        persist_if_truncated: bool,
    ) -> Result<CoreOutputSnapshot, ToolError> {
        self.tail.text();
        let at_boundary = self.tail.starts_at_line_boundary();
        let text = self.tail.text();
        let snapshot_text = if at_boundary {
            text
        } else {
            text.find('\n').map_or(text, |n| &text[n + 1..])
        };
        let mut truncation = truncate_tail(
            snapshot_text,
            TruncationOptions {
                max_lines: self.max_lines,
                max_bytes: self.max_bytes,
            },
        );
        let truncated =
            self.total_lines > self.max_lines || self.total_decoded_bytes > self.max_bytes;
        truncation.truncated = truncated;
        if truncated && truncation.truncated_by.is_none() {
            truncation.truncated_by = Some(
                if self.total_decoded_bytes > self.max_bytes {
                    "bytes"
                } else {
                    "lines"
                }
                .to_owned(),
            );
        }
        truncation.total_lines = self.total_lines;
        truncation.total_bytes = self.total_decoded_bytes;

        if persist_if_truncated && truncation.truncated {
            self.ensure_temp_file()?;
        }

        Ok(CoreOutputSnapshot {
            content: truncation.content.clone(),
            truncation,
            full_output_path: self.spill.as_ref().map(|s| s.path().to_path_buf()),
        })
    }

    pub async fn close_temp_file(&mut self) -> Result<(), ToolError> {
        if !self.spill_open {
            return Ok(());
        }
        self.spill_open = false;
        let close_res = match self.spill.as_mut() {
            Some(spill) => spill.close().await,
            None => return Ok(()),
        };
        if let Err(err) = close_res {
            if let Err(unlink_err) = self.remove_temp_file().await {
                return Err(ToolError::Message(format!(
                    "Output spill cleanup failed: {err}; {unlink_err}"
                )));
            }
            return Err(err);
        }
        Ok(())
    }

    pub async fn read_full_output(
        &mut self,
        max_bytes: usize,
    ) -> Result<CoreFullOutput, ToolError> {
        let Some(spill) = &mut self.spill else {
            let total: usize = self.raw_chunks.iter().map(Vec::len).sum();
            let mut joined = Vec::with_capacity(total);
            for chunk in &self.raw_chunks {
                joined.extend_from_slice(chunk);
            }
            if joined.len() <= max_bytes {
                return Ok(CoreFullOutput {
                    content: String::from_utf8_lossy(&joined).into_owned(),
                    truncated: false,
                });
            }
            let head_bytes = max_bytes / 2;
            let tail_bytes = max_bytes - head_bytes;
            let omitted = joined.len() - head_bytes - tail_bytes;
            return Ok(CoreFullOutput {
                content: join_head_and_tail(
                    &joined[..head_bytes],
                    &joined[joined.len() - tail_bytes..],
                    omitted,
                ),
                truncated: true,
            });
        };
        spill.read_full(max_bytes).await
    }

    pub fn get_last_line_bytes(&self) -> usize {
        self.current_line_bytes
    }

    pub async fn remove_temp_file(&mut self) -> Result<(), ToolError> {
        let Some(mut spill) = self.spill.take() else {
            return Ok(());
        };
        self.spill_open = false;
        if let Err(err) = spill.remove().await {
            self.spill = Some(spill);
            return Err(err);
        }
        Ok(())
    }

    fn append_decoded_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.tail
            .append(text, text.len(), self.current_line_bytes == 0);
        self.total_decoded_bytes += text.len();
        self.completed_lines += text.bytes().filter(|&b| b == b'\n').count();
        if let Some(idx) = text.rfind('\n') {
            self.current_line_bytes = text.len() - idx - 1;
        } else {
            self.current_line_bytes += text.len();
        }
        self.total_lines = self.completed_lines + usize::from(self.current_line_bytes > 0);
    }

    fn should_use_temp_file(&self) -> bool {
        self.total_raw_bytes > self.max_bytes
            || self.total_decoded_bytes > self.max_bytes
            || self.total_lines > self.max_lines
    }

    fn ensure_temp_file(&mut self) -> Result<(), ToolError> {
        if self.spill.is_some() {
            return Ok(());
        }
        let mut spill = (self.create_spill)(&self.temp_file_prefix)?;
        for chunk in self.raw_chunks.drain(..) {
            spill.write(&chunk);
        }
        self.spill = Some(spill);
        self.spill_open = true;
        Ok(())
    }
}
