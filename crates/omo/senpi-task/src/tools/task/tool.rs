//! `tools/task/tool.ts`: assembles the task tool definition: the parameter schema, a description
//! whose category and agent lists are injected dynamically from omo.json + the loader,
//! prompt-surface hints, the spawn execute logic, and compact call/result renderers.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::agents::AgentDefinition;
use crate::manager::{AbortSignal, WaitError};
use crate::tools::render::{LinesComponent, RendererTheme, ThemeColor};
use crate::tools::task::argument_normalization::normalize_task_tool_arguments;
use crate::tools::task::call_renderer::{TaskCallArgs, render_task_call_lines};
use crate::tools::task::description::{
    DescriptionInput, TASK_PROMPT_GUIDELINES, TASK_PROMPT_SNIPPET, build_task_tool_description,
};
use crate::tools::task::execute::{TaskExecute, build_task_execute};
use crate::tools::task::execute_batch::TaskToolResult;
use crate::tools::task::execute_single::{TaskExecuteDeps, TaskToolUpdate};
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::params::TASK_TOOL_PARAMS;
use crate::tools::task::renderers::render_task_result_component;
use crate::tools::task::validation::SpawnParamsInput;

pub const TASK_TOOL_NAME: &str = "task";

/// Everything the task tool needs: execute deps plus the inputs for the dynamic description.
pub struct TaskToolDeps<'a> {
    pub execute: TaskExecuteDeps<'a>,
    pub options: ForegroundWaitOptions,
    pub omo_config: &'a Value,
    pub agents: &'a BTreeMap<String, AgentDefinition>,
}

/// Options passed to `render_result` by the host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderResultOptions {
    pub is_partial: bool,
}

/// A fixed list of lines, independent of width.
pub struct StaticLinesComponent {
    lines: Vec<String>,
}

impl LinesComponent for StaticLinesComponent {
    fn render(&self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }
}

/// The call row: the task call lines, each tinted with `toolTitle`.
pub struct TaskCallComponent<'t> {
    args: &'t TaskCallArgs,
    theme: &'t dyn RendererTheme,
}

impl LinesComponent for TaskCallComponent<'_> {
    fn render(&self, width: usize) -> Vec<String> {
        render_task_call_lines(self.args, self.theme, Some(width))
            .into_iter()
            .map(|line| self.theme.fg(ThemeColor::ToolTitle, &line))
            .collect()
    }
}

/// The assembled task tool definition.
pub struct TaskTool<'a> {
    pub name: &'static str,
    pub label: &'static str,
    pub description: String,
    pub prompt_snippet: &'static str,
    pub prompt_guidelines: Vec<String>,
    pub parameters: Value,
    execute: TaskExecute<'a>,
}

pub fn create_task_tool(deps: TaskToolDeps<'_>) -> TaskTool<'_> {
    let description = build_task_tool_description(&DescriptionInput {
        omo_config: deps.omo_config,
        agents: deps.agents,
    });
    let execute = build_task_execute(deps.execute, deps.options);
    TaskTool {
        name: TASK_TOOL_NAME,
        label: "Task",
        description,
        prompt_snippet: TASK_PROMPT_SNIPPET,
        prompt_guidelines: TASK_PROMPT_GUIDELINES
            .iter()
            .map(|line| (*line).to_string())
            .collect(),
        parameters: TASK_TOOL_PARAMS.clone(),
        execute,
    }
}

impl TaskTool<'_> {
    pub fn prepare_arguments(&self, raw: &Value) -> Value {
        normalize_task_tool_arguments(raw)
    }

    pub fn execute(
        &self,
        tool_call_id: &str,
        params: &SpawnParamsInput,
        signal: Option<&AbortSignal>,
        on_update: Option<Arc<TaskToolUpdate>>,
        ctx: &TaskToolContext,
    ) -> Result<TaskToolResult, WaitError> {
        self.execute
            .execute(tool_call_id, params, signal, on_update, ctx)
    }

    pub fn render_call<'t>(
        &self,
        args: &'t TaskCallArgs,
        theme: &'t dyn RendererTheme,
    ) -> Box<dyn LinesComponent + 't> {
        Box::new(TaskCallComponent { args, theme })
    }

    pub fn render_result<'t>(
        &self,
        result: &TaskToolResult,
        options: RenderResultOptions,
        theme: &'t dyn RendererTheme,
    ) -> Box<dyn LinesComponent + 't> {
        if options.is_partial {
            // The live status line is drawn by senpi's own tool-progress renderer from
            // details.progress; the partial content carries only the last-assistant row.
            let live_text = result
                .content
                .iter()
                .find(|part| part.kind == "text")
                .map(|part| part.text.as_str());
            if let Some(live_text) = live_text.filter(|text| !text.is_empty()) {
                return Box::new(StaticLinesComponent {
                    lines: live_text
                        .split('\n')
                        .map(|line| theme.fg(ThemeColor::Muted, line))
                        .collect(),
                });
            }
            return Box::new(StaticLinesComponent { lines: Vec::new() });
        }
        Box::new(render_task_result_component(&result.details, theme))
    }
}
