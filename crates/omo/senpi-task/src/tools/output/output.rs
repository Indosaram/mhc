//! Port of `tools/output/output.ts`.

use std::io;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::manager::types::ListScope;
use crate::state::TaskRecord;
use crate::tools::control::caller_session::default_resolve_caller_session_id;
use crate::tools::control::tool_result::tool_result;
use crate::tools::control::types::{CallerSessionResolver, SessionIdCarrier};
use crate::tools::output::render::{RenderOptions, render_transcript};
use crate::tools::output::renderers::{
    OutputCallComponent, OutputLinesComponent, ToolRenderResultOptions, render_task_output_call,
    render_task_output_result, task_output_model_text,
};
use crate::tools::output::snapshot::build_task_snapshot;
use crate::tools::output::transcript::reader::default_transcript_reader;
use crate::tools::output::types::{
    OutputManager, TaskOutputDeps, TaskOutputDetails, TaskOutputToolResult, TaskSnapshot, TranscriptMode,
    TranscriptReaderInput,
};
use crate::tools::render::RendererTheme;

pub const TASK_OUTPUT_TOOL_NAME: &str = "task_output";
pub const TASK_OUTPUT_TOOL_LABEL: &str = "Task Output";

/// `mode` of `TaskOutputParams`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskOutputMode {
    Status,
    Tail,
    Full,
}

impl TaskOutputMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Tail => "tail",
            Self::Full => "full",
        }
    }
}

/// `Static<typeof TaskOutputParams>`. The legacy `block`/`timeout_ms` params are captured so they can
/// be rejected with guidance (the TS reads them via `Reflect.get`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskOutputInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<TaskOutputMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<Value>,
}

/// JSON schema equivalent of the TypeBox `TaskOutputParams` definition.
pub fn task_output_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "task_id": {
                "description": "Task id (st_...) of the child to read. Provide exactly one of task_id or name.",
                "type": "string"
            },
            "name": {
                "description": "Canonical task name; required if task_id is omitted.",
                "type": "string"
            },
            "mode": {
                "description": "status (default) = record snapshot + final result; tail = last lines of the transcript; full = whole transcript.",
                "anyOf": [
                    { "const": "status", "type": "string" },
                    { "const": "tail", "type": "string" },
                    { "const": "full", "type": "string" }
                ]
            },
            "tail_lines": {
                "minimum": 1,
                "description": "Lines to keep in tail mode. Defaults to 60.",
                "type": "integer"
            }
        }
    })
}

const DEFAULT_TAIL_LINES: usize = 60;
const BLOCKING_REMOVED_GUIDANCE: &str =
    "blocking removed - completion arrives as a notification; use mode:\"tail\" to peek.";

pub const TASK_OUTPUT_DESCRIPTION: &str = concat!(
    "Read one child task, keyed by task_id or name. task_output always returns immediately: mode='status' (default) returns the record snapshot plus the final response once terminal.",
    " ",
    "mode='tail' returns the last tail_lines of the recorded transcript; mode='full' returns the whole transcript (capped, with a head/tail elision marker). Completion notifications already include the final result.",
    " ",
    "READ-ONLY: this never revives, steers, or otherwise touches the child. A lost task returns a status view with a lost explanation and pid/session-dir breadcrumbs.",
    " ",
    "Only the current session's children are visible.",
);

/// Transcript reader I/O failures propagate (the TS rejects).
pub fn run_task_output(
    deps: &TaskOutputDeps,
    params: &TaskOutputInput,
    caller_session_id: Option<&str>,
) -> io::Result<TaskOutputToolResult> {
    if has_legacy_blocking_param(params) {
        return Ok(invalid_arguments(BLOCKING_REMOVED_GUIDANCE));
    }

    let Some(id_or_name) = params.task_id.as_deref().or(params.name.as_deref()) else {
        return Ok(invalid_arguments("Provide task_id or name to identify the child task."));
    };

    let candidates = scoped_candidates(deps.manager.as_ref(), caller_session_id);
    let Some(record) = resolve_target(&candidates, id_or_name) else {
        return Ok(not_found(&candidates, id_or_name));
    };

    output_for_record(deps, record, params)
}

fn has_legacy_blocking_param(params: &TaskOutputInput) -> bool {
    params.block.is_some() || params.timeout_ms.is_some()
}

fn output_for_record(
    deps: &TaskOutputDeps,
    record: &TaskRecord,
    params: &TaskOutputInput,
) -> io::Result<TaskOutputToolResult> {
    let now = match &deps.now {
        Some(clock) => clock(),
        None => chrono::Utc::now().timestamp_millis(),
    };
    let snapshot = build_task_snapshot(record, &deps.state_dir, now);
    let mode = params.mode.unwrap_or(TaskOutputMode::Status);

    let transcript_mode = match mode {
        TaskOutputMode::Tail => TranscriptMode::Tail,
        TaskOutputMode::Full => TranscriptMode::Full,
        TaskOutputMode::Status => {
            return Ok(tool_result(&status_text(&snapshot), TaskOutputDetails::Status { snapshot }));
        }
    };
    if record.status.as_str() == "lost" {
        return Ok(tool_result(&status_text(&snapshot), TaskOutputDetails::Status { snapshot }));
    }

    transcript_result(
        deps,
        record,
        snapshot,
        transcript_mode,
        params.tail_lines.unwrap_or(DEFAULT_TAIL_LINES),
    )
}

fn transcript_result(
    deps: &TaskOutputDeps,
    record: &TaskRecord,
    snapshot: TaskSnapshot,
    mode: TranscriptMode,
    tail_lines: usize,
) -> io::Result<TaskOutputToolResult> {
    let input = TranscriptReaderInput {
        task_id: &record.task_id,
        state_dir: &deps.state_dir,
    };
    let read = match &deps.transcript_reader {
        Some(reader) => reader(&input)?,
        None => default_transcript_reader(&input)?,
    };
    let rendered = render_transcript(&read.entries, &RenderOptions { mode, tail_lines });
    let text = format!(
        "{} [{}] transcript via {}:\n{}",
        record.task_id,
        record.status.as_str(),
        read.source.as_str(),
        rendered.text
    );
    let details = TaskOutputDetails::Transcript {
        mode,
        source: read.source,
        transcript: rendered.text,
        truncated: rendered.truncated || read.truncated == Some(true),
        snapshot,
    };
    Ok(tool_result(&text, details))
}

// Fail-closed scope: candidates are ONLY the caller session's children. No caller id -> nothing is
// visible, so a valid id owned by another session reads as not_found (never cross-session leakage).
fn scoped_candidates(manager: &dyn OutputManager, caller_session_id: Option<&str>) -> Vec<TaskRecord> {
    let Some(session_id) = caller_session_id else {
        return Vec::new();
    };
    manager
        .list(&ListScope::ParentSession(session_id.to_string()))
        .into_iter()
        .map(|entry| entry.record)
        .collect()
}

fn resolve_target<'a>(candidates: &'a [TaskRecord], id_or_name: &str) -> Option<&'a TaskRecord> {
    candidates
        .iter()
        .find(|record| record.task_id == id_or_name)
        .or_else(|| candidates.iter().find(|record| record.name.as_deref() == Some(id_or_name)))
}

fn status_text(snapshot: &TaskSnapshot) -> String {
    let mut parts = vec![format!(
        "{} [{}] {}",
        snapshot.task_id,
        snapshot.status.as_str(),
        task_output_model_text(snapshot)
    )];
    if let Some(suspended) = &snapshot.suspended {
        parts.push(suspended.explanation.clone());
    }
    if let Some(pid) = snapshot.pid {
        parts.push(format!("pid {pid}"));
    }
    if let Some(lost) = &snapshot.lost {
        parts.push(lost.explanation.clone());
    }
    if let Some(error) = &snapshot.error_message {
        parts.push(format!("error: {error}"));
    }
    if let Some(final_response) = &snapshot.final_response {
        parts.push(final_response.clone());
    }
    parts.join("\n")
}

fn not_found(candidates: &[TaskRecord], id_or_name: &str) -> TaskOutputToolResult {
    let known: Vec<String> = candidates
        .iter()
        .map(|record| record.name.clone().unwrap_or_else(|| record.task_id.clone()))
        .collect();
    let list_text = if known.is_empty() {
        String::new()
    } else {
        format!(" Known tasks in this session: {}.", known.join(", "))
    };
    let reason = format!("No task '{id_or_name}' in this session.");
    tool_result(
        &format!("{reason}{list_text}"),
        TaskOutputDetails::NotFound {
            reason,
            known_tasks: known,
        },
    )
}

fn invalid_arguments(reason: &str) -> TaskOutputToolResult {
    tool_result(
        reason,
        TaskOutputDetails::InvalidArguments {
            reason: reason.to_string(),
        },
    )
}

/// Minimal model of the `task_output` `ToolDefinition`.
pub struct TaskOutputTool {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    pub deps: TaskOutputDeps,
    pub resolve_caller: CallerSessionResolver,
}

impl TaskOutputTool {
    pub fn execute(
        &self,
        _tool_call_id: &str,
        params: &TaskOutputInput,
        ctx: &dyn SessionIdCarrier,
    ) -> io::Result<TaskOutputToolResult> {
        let caller_session_id = (self.resolve_caller)(ctx);
        run_task_output(&self.deps, params, caller_session_id.as_deref())
    }

    pub fn render_call<'a>(&self, args: &TaskOutputInput, theme: &'a dyn RendererTheme) -> OutputCallComponent<'a> {
        render_task_output_call(args, theme)
    }

    pub fn render_result(
        &self,
        result: &TaskOutputToolResult,
        options: &ToolRenderResultOptions,
        theme: &dyn RendererTheme,
    ) -> OutputLinesComponent {
        render_task_output_result(result, options, theme)
    }
}

fn default_caller_resolver() -> CallerSessionResolver {
    Arc::new(default_resolve_caller_session_id)
}

pub fn create_task_output_tool(deps: TaskOutputDeps) -> TaskOutputTool {
    let resolve_caller = deps
        .resolve_caller_session_id
        .clone()
        .unwrap_or_else(default_caller_resolver);
    TaskOutputTool {
        name: TASK_OUTPUT_TOOL_NAME,
        label: TASK_OUTPUT_TOOL_LABEL,
        description: TASK_OUTPUT_DESCRIPTION,
        parameters: task_output_params_schema(),
        deps,
        resolve_caller,
    }
}
