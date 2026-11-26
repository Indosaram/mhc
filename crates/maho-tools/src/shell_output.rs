//! Port of `packages/coding-agent/src/core/tools/shell-output.ts`.

use crate::bash_schema::BashToolDetails;
use crate::definition::{ToolContent, ToolError, ToolResult, ToolUpdateCallback};
use crate::model_only_text::model_only_text;
use crate::output_accumulator_core::{CoreOutputAccumulator, CoreOutputSnapshot};
use crate::secret_output_redactor::RedactedStream;
use crate::truncate::{DEFAULT_MAX_BYTES, format_size};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BASH_UPDATE_THROTTLE: Duration = Duration::from_millis(100);

pub struct FormattedShellOutput {
    pub text: String,
    pub content: Vec<ToolContent>,
    pub details: Option<BashToolDetails>,
}

pub struct ShellOutputState {
    pub accumulator: CoreOutputAccumulator,
    streams: Option<HashMap<String, RedactedStream>>,
    accepting_output: bool,
    update_dirty: bool,
    last_update_at: Option<Instant>,
    output_update_error: Option<ToolError>,
    on_update: Option<ToolUpdateCallback>,
}

impl ShellOutputState {
    pub fn new(
        accumulator: CoreOutputAccumulator,
        on_update: Option<ToolUpdateCallback>,
        redact_secrets_in_output: bool,
    ) -> Self {
        Self {
            accumulator,
            streams: redact_secrets_in_output.then(HashMap::new),
            accepting_output: true,
            update_dirty: false,
            last_update_at: None,
            output_update_error: None,
            on_update,
        }
    }

    pub fn emit_output_update(&mut self) {
        let Some((on_update, result)) = self.take_output_update() else {
            return;
        };
        if let Err(err) = on_update(result) {
            self.record_output_update_error(err);
        }
    }

    pub fn take_output_update(&mut self) -> Option<(ToolUpdateCallback, ToolResult)> {
        let on_update = self.on_update.clone()?;
        if !self.update_dirty || self.output_update_error.is_some() {
            return None;
        }
        self.update_dirty = false;
        self.last_update_at = Some(Instant::now());
        match self.accumulator.snapshot(true) {
            Ok(snapshot) => {
                let truncation = snapshot
                    .truncation
                    .truncated
                    .then_some(&snapshot.truncation);
                let result = ToolResult {
                    content: vec![ToolContent::text(snapshot.content)],
                    details: Some(json!({
                        "truncation": truncation,
                        "fullOutputPath": snapshot.full_output_path,
                    })),
                    structured_content: None,
                    is_error: None,
                };
                Some((on_update, result))
            }
            Err(err) => {
                self.output_update_error = Some(err);
                None
            }
        }
    }

    pub fn take_due_output_update(&mut self) -> Option<(ToolUpdateCallback, ToolResult)> {
        let should_emit = self
            .last_update_at
            .is_none_or(|prev| prev.elapsed() >= BASH_UPDATE_THROTTLE);
        should_emit.then(|| self.take_output_update()).flatten()
    }

    pub fn record_output_update_error(&mut self, error: ToolError) {
        if self.output_update_error.is_none() {
            self.output_update_error = Some(error);
        }
    }

    pub fn handle_data(&mut self, data: &[u8], stream: &str) -> Result<(), ToolError> {
        if !self.accepting_output {
            return Ok(());
        }
        if let Some(streams) = &mut self.streams {
            let entry = streams.entry(stream.to_owned()).or_default();
            let redacted = entry.push_bytes(data);
            self.accumulator.append_text(&redacted)?;
        } else {
            self.accumulator.append(data)?;
        }
        self.update_dirty = true;
        Ok(())
    }

    pub fn is_update_dirty(&self) -> bool {
        self.update_dirty && self.on_update.is_some() && self.output_update_error.is_none()
    }

    pub fn next_update_deadline(&self) -> Option<tokio::time::Instant> {
        if !self.is_update_dirty() {
            return None;
        }
        let target = match self.last_update_at {
            Some(prev) => tokio::time::Instant::from_std(prev + BASH_UPDATE_THROTTLE),
            None => tokio::time::Instant::now(),
        };
        Some(target)
    }

    pub async fn finish_output(&mut self) -> Result<CoreOutputSnapshot, ToolError> {
        self.accepting_output = false;
        let mut primary_error: Option<ToolError> = None;
        let mut snapshot: Option<CoreOutputSnapshot> = None;

        let prep_res: Result<CoreOutputSnapshot, ToolError> = (|| {
            if let Some(streams) = &mut self.streams {
                for state in streams.values_mut() {
                    let flushed = state.finish_stream();
                    self.accumulator.append_text(&flushed)?;
                }
                self.update_dirty = true;
            }
            self.accumulator.finish()?;
            self.emit_output_update();
            let snap = self.accumulator.snapshot(true)?;
            if let Some(err) = self.output_update_error.take() {
                return Err(err);
            }
            Ok(snap)
        })();

        match prep_res {
            Ok(snap) => snapshot = Some(snap),
            Err(err) => primary_error = Some(err),
        }

        if let Err(close_err) = self.accumulator.close_temp_file().await {
            if let Some(primary) = primary_error {
                return Err(ToolError::Message(format!(
                    "{primary}; Bash output finalization and spill cleanup failed: {close_err}"
                )));
            }
            return Err(close_err);
        }

        if let Some(primary) = primary_error {
            if let Err(unlink_err) = self.accumulator.remove_temp_file().await {
                return Err(ToolError::Message(format!(
                    "{primary}; Bash output finalization and spill cleanup failed: {unlink_err}"
                )));
            }
            return Err(primary);
        }

        snapshot.ok_or_else(|| {
            ToolError::Message("Bash output finalization produced no snapshot".to_owned())
        })
    }

    pub fn format_output(
        &self,
        snapshot: &CoreOutputSnapshot,
        empty_text: &str,
    ) -> FormattedShellOutput {
        let truncation = &snapshot.truncation;
        let text = if snapshot.content.is_empty() {
            empty_text
        } else {
            &snapshot.content
        };
        let mut notice: Option<String> = None;
        let mut details: Option<BashToolDetails> = None;
        if truncation.truncated {
            details = Some(BashToolDetails {
                truncation: Some(truncation.clone()),
                full_output_path: snapshot.full_output_path.clone(),
                trust: None,
            });
            let path = snapshot
                .full_output_path
                .as_ref()
                .map_or_else(|| "undefined".to_owned(), |p| p.display().to_string());
            let start_line = truncation.total_lines - truncation.output_lines + 1;
            let end_line = truncation.total_lines;
            notice = Some(if truncation.last_line_partial {
                let last_line_size = format_size(self.accumulator.get_last_line_bytes());
                format!(
                    "[Showing last {} of line {end_line} (line is {last_line_size}). Full output: {path}]",
                    format_size(truncation.output_bytes)
                )
            } else if truncation.truncated_by.as_deref() == Some("lines") {
                format!(
                    "[Showing lines {start_line}-{end_line} of {}. Full output: {path}]",
                    truncation.total_lines
                )
            } else {
                format!(
                    "[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {path}]",
                    truncation.total_lines,
                    format_size(DEFAULT_MAX_BYTES)
                )
            });
        }
        let mut content = vec![ToolContent::text(match &notice {
            Some(_) => format!("{text}\n"),
            None => text.to_owned(),
        })];
        let combined = match notice {
            Some(n) => {
                content.push(model_only_text(&n));
                format!("{text}\n\n{n}")
            }
            None => text.to_owned(),
        };
        FormattedShellOutput {
            text: combined,
            content,
            details,
        }
    }
}

pub fn shared_shell_output(
    accumulator: CoreOutputAccumulator,
    on_update: Option<ToolUpdateCallback>,
    redact_secrets_in_output: bool,
) -> Arc<Mutex<ShellOutputState>> {
    Arc::new(Mutex::new(ShellOutputState::new(
        accumulator,
        on_update,
        redact_secrets_in_output,
    )))
}
