//! Port of `tools/control/cancel.ts`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::state::TaskStatus;
use crate::tools::control::renderers::{
    ControlCallComponent, ControlLinesComponent, ToolRenderResultOptions, render_task_cancel_call,
    render_task_cancel_result,
};
use crate::tools::control::tool_result::tool_result;
use crate::tools::control::types::{CancelManager, CancelOutcome, CancelResultDetails, CancelToolResult};
use crate::tools::render::RendererTheme;

pub const TASK_CANCEL_TOOL_NAME: &str = "task_cancel";
pub const TASK_CANCEL_TOOL_LABEL: &str = "Task Cancel";

const MISSING_TARGET_REASON: &str = "Provide task_id or name to identify the child task.";

pub const TASK_CANCEL_DESCRIPTION: &str = concat!(
    "Cancel a running child task and release its resources; the cancelled status is preserved so task_output can still report the outcome.",
    " ",
    "Cancel is terminal and NOT resumable; cancelling a child that is not running is a no-op that reports its unchanged status.",
    " ",
    "Use this to end work you no longer need.",
);

/// JSON schema equivalent of the TypeBox `TaskCancelParams` definition.
pub fn task_cancel_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "task_id": {
                "description": "Task id (st_...) of the child to cancel. Provide exactly one of task_id or name.",
                "type": "string"
            },
            "name": {
                "description": "Canonical task name; required if task_id is omitted.",
                "type": "string"
            },
            "reason": {
                "description": "Optional human-readable reason recorded on the cancelled task.",
                "type": "string"
            }
        }
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCancelInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone)]
pub struct TaskCancelDeps {
    pub manager: Arc<dyn CancelManager>,
}

pub fn run_task_cancel(manager: &dyn CancelManager, params: &TaskCancelInput) -> CancelToolResult {
    let Some(id_or_name) = params.task_id.as_deref().or(params.name.as_deref()) else {
        return tool_result(
            MISSING_TARGET_REASON,
            CancelResultDetails::InvalidArguments {
                reason: MISSING_TARGET_REASON.to_string(),
            },
        );
    };

    match manager.cancel_task(id_or_name, params.reason.as_deref()) {
        CancelOutcome::Cancelled {
            task_id,
            previous_status,
        } => {
            let status = manager
                .get_status(&task_id)
                .or_else(|| TaskStatus::parse("cancelled"))
                .unwrap_or(previous_status);
            tool_result(
                &format!(
                    "Cancelled {task_id} (was {}, now {}).",
                    previous_status.as_str(),
                    status.as_str()
                ),
                CancelResultDetails::Cancelled {
                    task_id,
                    previous_status,
                    status,
                },
            )
        }
        CancelOutcome::Noop {
            task_id,
            status,
            reason,
        } => tool_result(
            &format!("{reason} No change."),
            CancelResultDetails::Noop {
                task_id,
                status,
                reason,
            },
        ),
        CancelOutcome::NotFound { reason } => tool_result(
            &reason,
            CancelResultDetails::NotFound {
                reason: reason.clone(),
            },
        ),
    }
}

/// Minimal model of the `task_cancel` `ToolDefinition`.
pub struct TaskCancelTool {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    deps: TaskCancelDeps,
}

impl TaskCancelTool {
    pub fn execute(&self, _tool_call_id: &str, params: &TaskCancelInput) -> CancelToolResult {
        run_task_cancel(self.deps.manager.as_ref(), params)
    }

    pub fn render_call<'a>(&self, args: &TaskCancelInput, theme: &'a dyn RendererTheme) -> ControlCallComponent<'a> {
        render_task_cancel_call(args, theme)
    }

    pub fn render_result(
        &self,
        result: &CancelToolResult,
        options: &ToolRenderResultOptions,
        theme: &dyn RendererTheme,
    ) -> ControlLinesComponent {
        render_task_cancel_result(result, options, theme)
    }
}

pub fn create_task_cancel_tool(deps: TaskCancelDeps) -> TaskCancelTool {
    TaskCancelTool {
        name: TASK_CANCEL_TOOL_NAME,
        label: TASK_CANCEL_TOOL_LABEL,
        description: TASK_CANCEL_DESCRIPTION,
        parameters: task_cancel_params_schema(),
        deps,
    }
}
