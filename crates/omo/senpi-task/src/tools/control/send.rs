//! Port of `tools/control/send.ts`.

use std::sync::Arc;

use serde_json::Value;

use crate::tools::control::caller_session::default_resolve_caller_session_id;
use crate::tools::control::renderers::{
    ControlCallComponent, ControlLinesComponent, ToolRenderResultOptions, render_member_scoped_task_send_call,
    render_task_send_call, render_task_send_result,
};
use crate::tools::control::send_results::{invalid_arguments, map_send_outcome, not_found};
use crate::tools::control::send_schema::{
    MemberScopedTaskSendInput, StructuredMessageInput, TaskSendInput, TaskSendMessage, as_structured_message,
    member_scoped_task_send_params_schema, task_send_params_schema,
};
use crate::tools::control::send_shutdown::{
    SendTeamRunIdResolution, TaskSendError, TaskSendTeamRouting, resolve_send_team_run_id, route_structured_message,
};
use crate::tools::control::tool_result::{AgentToolResult, ToolResultContent, tool_result};
use crate::tools::control::types::{
    CallerSessionResolver, ControlSendInput, SendManager, SendOutcome, SendResultDetails, SendToolResult,
    SessionIdCarrier,
};
use crate::tools::render::RendererTheme;
use crate::tools::team::messaging::{TeamSendDetails, TeamSendInput, run_team_send};
use crate::tools::team::types::TeamToolsService;

pub const TASK_SEND_TOOL_NAME: &str = "task_send";
pub const TASK_SEND_TOOL_LABEL: &str = "Task Send";

pub const TASK_SEND_DESCRIPTION: &str = concat!(
    "Send a message to a child task or team member, keyed by to.",
    " ",
    "Plain-text messages always steer a running child immediately.",
    " ",
    "A plain-text message to a finished resident child revives that same session; disposed, evicted, cancelled, and terminal-errored children are not revived.",
    " ",
    "message is required and accepts a plain string or a structured shutdown object {type:'shutdown_request'} or {type:'shutdown_response', approve, reason?}; structured messages are lead-only and need team_run_id (defaults to your single owned team).",
    " ",
    "To retire a member: send {type:'shutdown_request'}, then after it wraps up {type:'shutdown_response', approve:true}.",
    " ",
    "Addressing: a child task id/name goes to the live session; a team member name goes to the durable mailbox; '*' broadcasts to every member (lead-only). Plain-text bodies are capped by the team payload limit (default 32 KB); split larger payloads or send a file path.",
    " ",
    "Cross-session: a child owned by another session is refused unless you pass all_scope=true.",
    " ",
    "Team messages always steer into the recipient's running turn.",
    " ",
    "One-shot agents (momus) always refuse task_send in every state; spawn a new momus instead.",
);

pub const MEMBER_SCOPED_TASK_SEND_DESCRIPTION: &str = concat!(
    "Send a durable message to another team member or the team lead.",
    " ",
    "Team messages always steer into the recipient's running turn.",
);

pub struct TaskSendDeps {
    pub manager: Arc<dyn SendManager>,
    pub team_routing: Option<TaskSendTeamRouting>,
    pub resolve_caller_session_id: Option<CallerSessionResolver>,
}

pub fn run_task_send(
    manager: &dyn SendManager,
    params: &TaskSendInput,
    caller_session_id: Option<&str>,
    team_routing: Option<&TaskSendTeamRouting>,
) -> Result<SendToolResult, TaskSendError> {
    if let Some(validation) = validate_params(params) {
        return Ok(validation);
    }

    match &params.message {
        Some(TaskSendMessage::Plain(message)) => {
            let outcome = manager
                .send_to_task(&ControlSendInput {
                    id_or_name: params.to.clone(),
                    message: message.clone(),
                    caller_session_id: caller_session_id.map(str::to_string),
                    all_scope: (params.all_scope == Some(true)).then_some(true),
                })
                .map_err(TaskSendError::Manager)?;

            let reason = match outcome {
                SendOutcome::NotFound { reason } => reason,
                other => return Ok(map_send_outcome(other)?),
            };
            let Some(team_routing) = team_routing else {
                return Ok(not_found(manager, &reason, caller_session_id));
            };

            let team_run_id = match resolve_send_team_run_id(params, team_routing) {
                SendTeamRunIdResolution::None => return Ok(not_found(manager, &reason, caller_session_id)),
                SendTeamRunIdResolution::Error { reason } => return Ok(invalid_arguments(&reason)),
                SendTeamRunIdResolution::Resolved { team_run_id } => team_run_id,
            };

            let team_result = run_team_send(
                team_routing.service.as_ref(),
                &team_run_id,
                &team_routing.from,
                &TeamSendInput {
                    to: params.to.clone(),
                    body: message.clone(),
                    summary: params.summary.clone(),
                },
            )?;
            let text = first_text(&team_result);
            Ok(tool_result(
                &text,
                SendResultDetails::TeamMessage {
                    team: team_result.details,
                },
            ))
        }
        Some(TaskSendMessage::Structured(structured)) => {
            route_structured_message(&params.to, structured, params, team_routing)
        }
        None => Ok(invalid_arguments("message is required")),
    }
}

fn validate_params(params: &TaskSendInput) -> Option<SendToolResult> {
    let message = params.message.as_ref();
    if message.is_none() {
        return Some(invalid_arguments("message is required"));
    }
    if is_shutdown_reject_without_reason(message) {
        return Some(invalid_arguments("reason is required when rejecting a shutdown"));
    }
    None
}

fn is_shutdown_reject_without_reason(message: Option<&TaskSendMessage>) -> bool {
    matches!(
        as_structured_message(message),
        Some(StructuredMessageInput::ShutdownResponse { approve: false, reason, .. })
            if reason.as_deref().is_none_or(|reason| reason.trim().is_empty())
    )
}

fn first_text(result: &AgentToolResult<TeamSendDetails>) -> String {
    result
        .content
        .first()
        .map(|ToolResultContent::Text { text }| text.clone())
        .unwrap_or_else(|| "Team message sent.".to_string())
}

fn default_caller_resolver() -> CallerSessionResolver {
    Arc::new(default_resolve_caller_session_id)
}

/// The lead/child `task_send` tool definition.
pub struct TaskSendTool {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    pub deps: TaskSendDeps,
    pub resolve_caller: CallerSessionResolver,
}

impl TaskSendTool {
    pub fn execute(
        &self,
        _tool_call_id: &str,
        params: &TaskSendInput,
        ctx: &dyn SessionIdCarrier,
    ) -> Result<SendToolResult, TaskSendError> {
        let caller_session_id = (self.resolve_caller)(ctx);
        run_task_send(
            self.deps.manager.as_ref(),
            params,
            caller_session_id.as_deref(),
            self.deps.team_routing.as_ref(),
        )
    }

    pub fn render_call<'a>(&self, args: &TaskSendInput, theme: &'a dyn RendererTheme) -> ControlCallComponent<'a> {
        render_task_send_call(args, theme)
    }

    pub fn render_result(
        &self,
        result: &SendToolResult,
        options: &ToolRenderResultOptions,
        theme: &dyn RendererTheme,
    ) -> ControlLinesComponent {
        render_task_send_result(result, options, theme)
    }
}

pub fn create_task_send_tool(deps: TaskSendDeps) -> TaskSendTool {
    let resolve_caller = deps
        .resolve_caller_session_id
        .clone()
        .unwrap_or_else(default_caller_resolver);
    TaskSendTool {
        name: TASK_SEND_TOOL_NAME,
        label: TASK_SEND_TOOL_LABEL,
        description: TASK_SEND_DESCRIPTION,
        parameters: task_send_params_schema(),
        deps,
        resolve_caller,
    }
}

pub struct MemberScopedTaskSendDeps {
    pub manager: Arc<dyn SendManager>,
    pub service: Arc<dyn TeamToolsService>,
    pub team_run_id: String,
    pub from: String,
    pub resolve_caller_session_id: Option<CallerSessionResolver>,
}

/// The member-scoped `task_send` tool definition (plain-text only, bound to one team run).
pub struct MemberScopedTaskSendTool {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    pub manager: Arc<dyn SendManager>,
    pub team_routing: TaskSendTeamRouting,
    pub resolve_caller: CallerSessionResolver,
}

impl MemberScopedTaskSendTool {
    pub fn execute(
        &self,
        _tool_call_id: &str,
        params: &MemberScopedTaskSendInput,
        ctx: &dyn SessionIdCarrier,
    ) -> Result<SendToolResult, TaskSendError> {
        let caller_session_id = (self.resolve_caller)(ctx);
        let converted = TaskSendInput {
            to: params.to.clone(),
            message: Some(TaskSendMessage::Plain(params.message.clone())),
            team_run_id: None,
            summary: params.summary.clone(),
            all_scope: None,
        };
        run_task_send(
            self.manager.as_ref(),
            &converted,
            caller_session_id.as_deref(),
            Some(&self.team_routing),
        )
    }

    pub fn render_call<'a>(
        &self,
        args: &MemberScopedTaskSendInput,
        theme: &'a dyn RendererTheme,
    ) -> ControlCallComponent<'a> {
        render_member_scoped_task_send_call(args, theme)
    }

    pub fn render_result(
        &self,
        result: &SendToolResult,
        options: &ToolRenderResultOptions,
        theme: &dyn RendererTheme,
    ) -> ControlLinesComponent {
        render_task_send_result(result, options, theme)
    }
}

pub fn create_member_scoped_task_send_tool(deps: MemberScopedTaskSendDeps) -> MemberScopedTaskSendTool {
    let resolve_caller = deps
        .resolve_caller_session_id
        .unwrap_or_else(default_caller_resolver);
    MemberScopedTaskSendTool {
        name: TASK_SEND_TOOL_NAME,
        label: TASK_SEND_TOOL_LABEL,
        description: MEMBER_SCOPED_TASK_SEND_DESCRIPTION,
        parameters: member_scoped_task_send_params_schema(),
        manager: deps.manager,
        team_routing: TaskSendTeamRouting {
            service: deps.service,
            from: deps.from,
            team_run_id: Some(deps.team_run_id),
            resolve_default_team_run_id: None,
        },
        resolve_caller,
    }
}
