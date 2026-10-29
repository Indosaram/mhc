//! Port of `tools/team/lifecycle.ts`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::state::ResolvedModelRecord;
use crate::status_line::{StatusTargetInput, format_target_identity, format_target_with_model};
use crate::task_summary::TASK_SUMMARY_MAX_LENGTH;
use crate::team::errors::{SenpiTeamSpecError, SenpiTeamSpecErrorCode};
use crate::team::runtime_types::{
    CreatedMemberInfo, CreatedMemberRole, SenpiTeamRuntimeError, SenpiTeamRuntimeErrorCode,
};
use crate::tools::control::tool_result::{AgentToolResult, tool_result};
use crate::tools::team::index::TeamTool;
use crate::tools::team::types::{
    CreateTeamToolInput, DeleteTeamToolInput, TeamToolDeps, TeamToolServiceError, TeamToolsService,
};

pub const TEAM_CREATE_TOOL_NAME: &str = "team_create";
pub const TEAM_CREATE_TOOL_LABEL: &str = "Team Create";
pub const TEAM_DELETE_TOOL_NAME: &str = "team_delete";
pub const TEAM_DELETE_TOOL_LABEL: &str = "Team Delete";

const SPEC_ERROR_NAME: &str = "SenpiTeamSpecError";
const RUNTIME_ERROR_NAME: &str = "SenpiTeamRuntimeError";

pub const CREATE_DESCRIPTION: &str = concat!(
    "Create a team run from a named spec or an inline spec. The current session is the team lead.",
    " ",
    "Pass inline_spec for an ad hoc team or team_name for a named spec; inline_spec takes precedence when both are provided. Members run as background children; you coordinate them with the other team_* tools.",
    " ",
    "Returns invalid_arguments for malformed input, spec_error for invalid specs, and runtime_error for spawn/bounds failures.",
);

pub const DELETE_DESCRIPTION: &str = "Delete a team run and cancel its members (terminal, not resumable). Lead-only. Pass force=true to tear it down while members are still active.";

fn inline_team_spec_member_schema() -> Value {
    json!({
        "additionalProperties": true,
        "type": "object",
        "properties": {
            "name": {
                "description": "Member name; unique within the team, lowercase-stem normalized.",
                "type": "string"
            },
            "kind": {
                "description": "Member kind; 'agent' is an alias for subagent_type. Inferred from the category/subagent_type field when omitted.",
                "anyOf": [
                    { "const": "category", "type": "string" },
                    { "const": "subagent_type", "type": "string" },
                    { "const": "agent", "type": "string" }
                ]
            },
            "category": {
                "description": "Category to route this member through (kind category).",
                "type": "string"
            },
            "subagent_type": {
                "description": "Agent definition to run this member as (kind subagent_type or agent).",
                "type": "string"
            },
            "prompt": {
                "description": "Member instructions; MUST be written in English.",
                "type": "string"
            },
            "task_summary": {
                "maxLength": TASK_SUMMARY_MAX_LENGTH,
                "description": "One-line summary of this member's assigned work, shown in the task footer/widget UI. Longer values are force-truncated to 80 chars.",
                "type": "string"
            }
        }
    })
}

fn inline_team_spec_schema() -> Value {
    json!({
        "additionalProperties": true,
        "type": "object",
        "properties": {
            "name": {
                "description": "Team name; defaults to a derived inline name when omitted.",
                "type": "string"
            },
            "members": {
                "description": "Team members (an array, or a single member object which is wrapped into one). The current session is always the lead; do not declare a lead member.",
                "anyOf": [
                    { "type": "array", "items": inline_team_spec_member_schema() },
                    inline_team_spec_member_schema()
                ]
            }
        }
    })
}

/// JSON schema equivalent of the TypeBox `TeamCreateParams` definition.
pub fn team_create_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "team_name": {
                "description": "Named team spec (project .omo/teams or omo.json) to create. Ignored when inline_spec is also provided.",
                "type": "string"
            },
            "inline_spec": {
                "description": "Inline team spec, e.g. { name, members: [{ name, category|subagent_type, prompt? }] }. A JSON string of the same object is also accepted and parsed automatically. Takes precedence when team_name is also provided.",
                "anyOf": [
                    inline_team_spec_schema(),
                    {
                        "description": "The same spec as a JSON string; parsed automatically. Passing the object form is preferred.",
                        "type": "string"
                    }
                ]
            }
        }
    })
}

/// JSON schema equivalent of the TypeBox `TeamDeleteParams` definition.
pub fn team_delete_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "team_run_id": { "description": "Team run id to delete.", "type": "string" },
            "force": {
                "description": "Tear the run down even while members are still active.",
                "type": "boolean"
            }
        },
        "required": ["team_run_id"]
    })
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TeamCreateInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_spec: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamDeleteInput {
    pub team_run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeamCreateMemberView {
    pub name: String,
    pub status: String,
    pub role: String,
    pub task_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ResolvedModelRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamCreateDetails {
    Created {
        team_run_id: String,
        team_name: String,
        members: Vec<TeamCreateMemberView>,
    },
    InvalidArguments {
        reason: String,
    },
    SpecError {
        code: String,
        reason: String,
    },
    RuntimeError {
        code: String,
        reason: String,
    },
}

impl TeamCreateDetails {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Created { .. } => "created",
            Self::InvalidArguments { .. } => "invalid_arguments",
            Self::SpecError { .. } => "spec_error",
            Self::RuntimeError { .. } => "runtime_error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamDeleteDetails {
    Deleted {
        team_run_id: String,
        cancelled_task_ids: Vec<String>,
    },
    InvalidState {
        team_run_id: String,
        code: String,
        reason: String,
    },
}

impl TeamDeleteDetails {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Deleted { .. } => "deleted",
            Self::InvalidState { .. } => "invalid_state",
        }
    }
}

/// Carries a `SenpiTeamSpecError` across the service seam (`{ name, message }`).
pub fn spec_error_to_service_error(error: &SenpiTeamSpecError) -> TeamToolServiceError {
    TeamToolServiceError::with_code(SPEC_ERROR_NAME, error.message.clone(), error.code.as_str())
}

/// Carries a `SenpiTeamRuntimeError` across the service seam (`{ name, message }`).
pub fn runtime_error_to_service_error(error: &SenpiTeamRuntimeError) -> TeamToolServiceError {
    TeamToolServiceError::with_code(RUNTIME_ERROR_NAME, error.message.clone(), error.code.as_str())
}

/// Recovers the spec error code from the stable `SenpiTeamSpecError` messages (the service seam only
/// carries `{ name, message }`).
pub fn infer_spec_error_code(message: &str) -> SenpiTeamSpecErrorCode {
    if message.contains("passed a callerTeamLead option") {
        SenpiTeamSpecErrorCode::ReservedCallerTeamLead
    } else if message.contains("declares a 'lead' field") {
        SenpiTeamSpecErrorCode::ReservedLeadField
    } else if message.contains("has a member named 'lead'") {
        SenpiTeamSpecErrorCode::ReservedLeadMember
    } else if message.contains("references unknown category") {
        SenpiTeamSpecErrorCode::UnresolvableCategory
    } else if message.contains("references unknown subagent_type") || message.starts_with("curated read-only agent") {
        SenpiTeamSpecErrorCode::UnknownSubagentType
    } else {
        SenpiTeamSpecErrorCode::InvalidSpec
    }
}

/// Recovers the runtime error code from the stable `SenpiTeamRuntimeError` messages.
pub fn infer_runtime_error_code(message: &str) -> SenpiTeamRuntimeErrorCode {
    if message.contains("exceeding max_members") {
        SenpiTeamRuntimeErrorCode::BoundsExceeded
    } else if message.contains("creation exceeded max_wall_clock_minutes") {
        SenpiTeamRuntimeErrorCode::CreateDeadlineExceeded
    } else if message.contains("member sidecar write failed") {
        SenpiTeamRuntimeErrorCode::SidecarWriteFailed
    } else if message.contains("cannot be deleted from status") {
        SenpiTeamRuntimeErrorCode::InvalidDeleteState
    } else {
        SenpiTeamRuntimeErrorCode::MemberStartRejected
    }
}

fn coerce_inline_spec(input: &Value) -> Result<Value, String> {
    let Value::String(text) = input else {
        return Ok(input.clone());
    };
    serde_json::from_str::<Value>(text).map_err(|error| {
        format!(
            "inline_spec is a string that is not valid JSON ({error}). Pass the spec as a nested object like {{ name, members: [{{ name, category|subagent_type, prompt? }}] }}, or as a valid JSON string of that object."
        )
    })
}

fn member_status_text<T: Serialize>(status: T) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

pub fn run_team_create(
    service: &dyn TeamToolsService,
    params: &TeamCreateInput,
) -> Result<AgentToolResult<TeamCreateDetails>, TeamToolServiceError> {
    let has_name = params.team_name.as_deref().is_some_and(|name| !name.is_empty());
    let has_inline = params.inline_spec.is_some();
    if !has_name && !has_inline {
        return Ok(tool_result(
            "Provide team_name or inline_spec.",
            TeamCreateDetails::InvalidArguments {
                reason: "provide team_name or inline_spec".to_string(),
            },
        ));
    }

    let input = match &params.inline_spec {
        Some(raw) => match coerce_inline_spec(raw) {
            Ok(spec) => CreateTeamToolInput {
                team_name: None,
                inline_spec: Some(spec),
            },
            Err(reason) => {
                return Ok(tool_result(&reason, TeamCreateDetails::InvalidArguments { reason: reason.clone() }));
            }
        },
        None => CreateTeamToolInput {
            team_name: params.team_name.clone(),
            inline_spec: None,
        },
    };

    match service.create_team(&input) {
        Ok(result) => {
            let state = &result.runtime_state;
            let members: Vec<TeamCreateMemberView> = result
                .members
                .iter()
                .map(|member| TeamCreateMemberView {
                    name: member.name.clone(),
                    status: member_status_text(member.status),
                    role: format_member_role(&member.role),
                    task_id: member.task_id.clone(),
                    model: member.model.clone(),
                    prompt_excerpt: member.prompt_excerpt.clone(),
                    task_summary: member.task_summary.clone(),
                })
                .collect();
            let mut lines = vec![format!(
                "Created team '{}' ({}) with {} members.",
                state.team_name,
                state.team_run_id,
                members.len()
            )];
            lines.extend(result.members.iter().map(format_created_member_line));
            Ok(tool_result(
                &lines.join("\n"),
                TeamCreateDetails::Created {
                    team_run_id: state.team_run_id.clone(),
                    team_name: state.team_name.clone(),
                    members,
                },
            ))
        }
        Err(error) if error.name == SPEC_ERROR_NAME => Ok(tool_result(
            &error.message,
            TeamCreateDetails::SpecError {
                code: error.code.clone().unwrap_or_else(|| infer_spec_error_code(&error.message).as_str().to_string()),
                reason: error.message.clone(),
            },
        )),
        Err(error) if error.name == RUNTIME_ERROR_NAME => Ok(tool_result(
            &error.message,
            TeamCreateDetails::RuntimeError {
                code: error.code.clone().unwrap_or_else(|| infer_runtime_error_code(&error.message).as_str().to_string()),
                reason: error.message.clone(),
            },
        )),
        Err(error) => Err(error),
    }
}

pub fn run_team_delete(
    service: &dyn TeamToolsService,
    params: &TeamDeleteInput,
) -> Result<AgentToolResult<TeamDeleteDetails>, TeamToolServiceError> {
    match service.delete_team(&DeleteTeamToolInput {
        team_run_id: params.team_run_id.clone(),
        force: params.force,
    }) {
        Ok(result) => {
            let cancelled = &result.cancelled_task_ids;
            let suffix = if cancelled.is_empty() {
                String::new()
            } else {
                format!(": {}", cancelled.join(", "))
            };
            Ok(tool_result(
                &format!(
                    "Deleted team {}; cancelled {} member task(s){suffix}.",
                    result.team_run_id,
                    cancelled.len()
                ),
                TeamDeleteDetails::Deleted {
                    team_run_id: result.team_run_id.clone(),
                    cancelled_task_ids: result.cancelled_task_ids.clone(),
                },
            ))
        }
        Err(error) if error.name == RUNTIME_ERROR_NAME => Ok(tool_result(
            &error.message,
            TeamDeleteDetails::InvalidState {
                team_run_id: params.team_run_id.clone(),
                code: error.code.clone().unwrap_or_else(|| infer_runtime_error_code(&error.message).as_str().to_string()),
                reason: error.message.clone(),
            },
        )),
        Err(error) => Err(error),
    }
}

fn role_parts(role: &CreatedMemberRole) -> (Option<&str>, Option<&str>) {
    match role {
        CreatedMemberRole::Category { category } => (Some(category.as_str()), None),
        CreatedMemberRole::SubagentType { subagent_type } => (None, Some(subagent_type.as_str())),
    }
}

fn format_member_role(role: &CreatedMemberRole) -> String {
    let (category, agent_type) = role_parts(role);
    format_target_identity(category, agent_type).unwrap_or_else(|| "task".to_string())
}

// Prompt excerpts stay in details, never in the text lines: echoing raw member prompts into the
// lead conversation lets their content spoof scanners that match markers in the message stream
// (e2e role detection, keyword triggers).
fn format_created_member_line(member: &CreatedMemberInfo) -> String {
    let (category, agent_type) = role_parts(&member.role);
    let target = format_target_with_model(&StatusTargetInput {
        category,
        agent_type,
        resolved_model: member.model.as_ref(),
        ..Default::default()
    });
    format!(
        "- {} [{}] {} task:{}",
        member.name,
        member_status_text(member.status),
        target.unwrap_or_else(|| format_member_role(&member.role)),
        member.task_id
    )
}

pub fn create_team_create_tool(deps: &TeamToolDeps) -> TeamTool<TeamCreateInput, TeamCreateDetails> {
    TeamTool {
        name: TEAM_CREATE_TOOL_NAME,
        label: TEAM_CREATE_TOOL_LABEL,
        description: CREATE_DESCRIPTION,
        parameters: team_create_params_schema(),
        service: deps.service.clone(),
        run: run_team_create,
    }
}

pub fn create_team_delete_tool(deps: &TeamToolDeps) -> TeamTool<TeamDeleteInput, TeamDeleteDetails> {
    TeamTool {
        name: TEAM_DELETE_TOOL_NAME,
        label: TEAM_DELETE_TOOL_LABEL,
        description: DELETE_DESCRIPTION,
        parameters: team_delete_params_schema(),
        service: deps.service.clone(),
        run: run_team_delete,
    }
}
