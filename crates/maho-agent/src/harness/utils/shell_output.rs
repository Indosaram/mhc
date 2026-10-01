//! Port of senpi packages/agent/src/harness/utils/shell-output.ts.

use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;

use super::output_capture::{apply_shell_output_update, sanitize_shell_output};
use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncationOptions, TruncationResult, truncate_tail};
use crate::harness::context::Context;
use crate::harness::result::Result;
use crate::harness::types::{
    ExecutionEnv, ExecutionError, ShellExecOptions, ShellOutputCaptureOptions, ShellOutputLimits,
    ShellOutputRetention, ShellOutputUpdate, ShellOutputView,
};

/// `ShellCaptureProgress`.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellCaptureProgress {
    pub output: String,
    pub truncation: TruncationResult,
    pub full_output_path: Option<String>,
    pub last_line_bytes: u64,
}

/// `ShellCaptureOptions.onChunk`.
pub type ShellChunkHandler =
    Arc<dyn Fn(String, ShellCaptureProgress, Context) -> BoxFuture<'static, ()> + Send + Sync>;

/// `ShellCaptureOptions`.
#[derive(Default)]
pub struct ShellCaptureOptions {
    pub cwd: Option<String>,
    pub env: Option<serde_json::Map<String, serde_json::Value>>,
    pub inherit_env: Option<bool>,
    pub timeout: Option<f64>,
    pub on_chunk: Option<ShellChunkHandler>,
    pub return_execution_errors: Option<bool>,
}

/// `ShellCaptureResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellCaptureResult {
    pub output: String,
    pub truncation: TruncationResult,
    pub full_output_path: Option<String>,
    pub last_line_bytes: u64,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    pub execution_error: Option<ExecutionError>,
}

fn progress_from(output: &ShellOutputView) -> ShellCaptureProgress {
    ShellCaptureProgress {
        output: output.text.clone(),
        truncation: TruncationResult {
            content: output.text.clone(),
            truncated: output.truncation.truncated,
            truncated_by: output.truncation.truncated_by.map(|limit| match limit {
                crate::harness::types::TruncationLimit::Lines => super::truncate::TruncatedBy::Lines,
                crate::harness::types::TruncationLimit::Bytes => super::truncate::TruncatedBy::Bytes,
            }),
            total_lines: output.truncation.total_lines,
            total_bytes: output.truncation.total_bytes,
            output_lines: output.truncation.output_lines,
            output_bytes: output.truncation.output_bytes,
            last_line_partial: output.truncation.last_line_partial,
            first_line_exceeds_limit: output.truncation.first_line_exceeds_limit,
            max_lines: output.truncation.max_lines,
            max_bytes: output.truncation.max_bytes,
        },
        full_output_path: output.spill_path.clone(),
        last_line_bytes: output.last_line_bytes.unwrap_or(0),
    }
}

/// `executeShellWithCapture(env, command, options, context)`.
pub async fn execute_shell_with_capture(
    env: &dyn ExecutionEnv,
    command: &str,
    options: Option<ShellCaptureOptions>,
    context: &Context,
) -> Result<ShellCaptureResult, ExecutionError> {
    let output: Arc<Mutex<Option<ShellOutputView>>> = Arc::new(Mutex::new(None));
    let on_chunk = options.as_ref().and_then(|options| options.on_chunk.clone());
    let update_output = output.clone();
    let update_context = context.clone();

    let exec_options = ShellExecOptions {
        cwd: options.as_ref().and_then(|options| options.cwd.clone()),
        env: options.as_ref().and_then(|options| options.env.clone()),
        inherit_env: options.as_ref().and_then(|options| options.inherit_env),
        timeout: options.as_ref().and_then(|options| options.timeout),
        capture: Some(ShellOutputCaptureOptions {
            limits: ShellOutputLimits {
                max_bytes: DEFAULT_MAX_BYTES,
                max_lines: DEFAULT_MAX_LINES,
                retain: Some(ShellOutputRetention::Tail),
            },
            spill: Some(true),
        }),
        on_update: Some(Arc::new(move |update: ShellOutputUpdate, update_context: Context| {
            let previous = update_output.lock().expect("capture output poisoned").clone();
            let next = apply_shell_output_update(previous.as_ref(), &update);
            *update_output.lock().expect("capture output poisoned") = Some(next.clone());
            let chunk = match &update {
                ShellOutputUpdate::Append { text, .. } | ShellOutputUpdate::Slide { text, .. } => Some(text.clone()),
                ShellOutputUpdate::Replace { .. } if previous.is_none() => Some(next.text.clone()),
                _ => None,
            };
            let on_chunk = on_chunk.clone();
            let progress = progress_from(&next);
            Box::pin(async move {
                // A metadata-only update and a post-cap replacement contain no new incremental
                // chunk; the observer's settlement is awaited by the environment.
                if let (Some(chunk), Some(on_chunk)) = (chunk, on_chunk) {
                    on_chunk(chunk, progress, update_context).await;
                }
            })
        })),
    };

    let result = env.exec(command, Some(exec_options), context).await;
    let _ = update_context;

    let mut current = output.lock().expect("capture output poisoned").clone();
    if current.is_none() {
        let fallback = truncate_tail("", TruncationOptions::default());
        current = Some(ShellOutputView {
            text: fallback.content.clone(),
            truncation: crate::harness::types::ShellOutputTruncation {
                truncated: fallback.truncated,
                truncated_by: fallback.truncated_by.map(|limit| match limit {
                    super::truncate::TruncatedBy::Lines => crate::harness::types::TruncationLimit::Lines,
                    super::truncate::TruncatedBy::Bytes => crate::harness::types::TruncationLimit::Bytes,
                }),
                total_lines: fallback.total_lines,
                total_bytes: fallback.total_bytes,
                output_lines: fallback.output_lines,
                output_bytes: fallback.output_bytes,
                last_line_partial: fallback.last_line_partial,
                first_line_exceeds_limit: fallback.first_line_exceeds_limit,
                max_lines: fallback.max_lines,
                max_bytes: fallback.max_bytes,
            },
            spill_path: None,
            last_line_bytes: None,
        });
    }
    let current = current.expect("capture output is initialized above");
    let progress = progress_from(&current);

    match result {
        Err(error) => {
            if error.code == crate::harness::types::ExecutionErrorCode::Aborted || context.is_aborted() {
                return Ok(ShellCaptureResult {
                    output: progress.output,
                    truncation: progress.truncation.clone(),
                    full_output_path: progress.full_output_path,
                    last_line_bytes: progress.last_line_bytes,
                    exit_code: None,
                    cancelled: true,
                    truncated: progress.truncation.truncated,
                    execution_error: None,
                });
            }
            if options.as_ref().and_then(|options| options.return_execution_errors) == Some(true) {
                return Ok(ShellCaptureResult {
                    output: progress.output,
                    truncation: progress.truncation.clone(),
                    full_output_path: progress.full_output_path,
                    last_line_bytes: progress.last_line_bytes,
                    exit_code: None,
                    cancelled: false,
                    truncated: progress.truncation.truncated,
                    execution_error: Some(error),
                });
            }
            Err(error)
        }
        Ok(value) => Ok(ShellCaptureResult {
            output: progress.output,
            truncation: progress.truncation.clone(),
            full_output_path: progress.full_output_path,
            last_line_bytes: progress.last_line_bytes,
            exit_code: Some(value.exit_code),
            cancelled: false,
            truncated: value.truncation.truncated,
            execution_error: None,
        }),
    }
}

/// `sanitizeBinaryOutput` alias.
pub fn sanitize_binary_output(text: &str) -> String {
    sanitize_shell_output(text)
}
