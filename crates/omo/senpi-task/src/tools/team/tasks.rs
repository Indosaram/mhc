//! Port of `tools/team/tasks.ts`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use team_core::types::{Task, TaskStatus};

use crate::tools::control::tool_result::{AgentToolResult, tool_result};
use crate::tools::team::index::TeamTool;
use crate::tools::team::types::{
    CreateTeamTaskServiceInput, TeamTaskListFilter, TeamToolDeps, TeamToolServiceError, TeamToolsService,
    UpdateTeamTaskServiceInput,
};

pub const TASK_CREATE_TOOL_NAME: &str = "task_create";
pub const TASK_CREATE_TOOL_LABEL: &str = "Task Create";
pub const TASK_LIST_TOOL_NAME: &str = "task_list";
pub const TASK_LIST_TOOL_LABEL: &str = "Task List";
pub const TASK_GET_TOOL_NAME: &str = "task_get";
pub const TASK_GET_TOOL_LABEL: &str = "Task Get";
pub const TASK_UPDATE_TOOL_NAME: &str = "task_update";
pub const TASK_UPDATE_TOOL_LABEL: &str = "Task Update";

pub const TASK_CREATE_DESCRIPTION: &str = "Create an entry on the team tasklist (starts pending). Does NOT spawn an agent; use the 'task' tool to spawn child work.";
pub const TASK_LIST_DESCRIPTION: &str = "Team tasklist: list entries, optionally filtered by status (pending, claimed, in_progress, completed, deleted) or owner. Child-agent state lives in task_output, not here.";
pub const TASK_GET_DESCRIPTION: &str = "Read one team tasklist entry by id; returns not_found if absent. The task_id is a tasklist id, not a child st_... id; use task_output for child agents.";
pub const TASK_UPDATE_DESCRIPTION: &str = "Update a team tasklist entry's status. Transitions run pending -> claimed -> in_progress -> completed, with deleted allowed from any state; status='claimed' claims it for owner (defaults to the lead). Illegal moves return already_claimed, blocked_by, invalid_transition, or cross_owner.";

const ECHO_TEXT_MAX: usize = 160;
const ECHO_DESCRIPTION_MAX: usize = 1_000;

fn task_status_schema() -> Value {
    json!({
        "description": "Task status.",
        "anyOf": [
            { "const": "pending", "type": "string" },
            { "const": "claimed", "type": "string" },
            { "const": "in_progress", "type": "string" },
            { "const": "completed", "type": "string" },
            { "const": "deleted", "type": "string" }
        ]
    })
}

/// JSON schema equivalent of the TypeBox `TeamTaskCreateParams` definition.
pub fn team_task_create_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "team_run_id": { "description": "Team run id.", "type": "string" },
            "subject": { "description": "Short task subject.", "type": "string" },
            "description": { "description": "Full task description.", "type": "string" },
            "blocked_by": {
                "description": "Task ids that must complete first.",
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["team_run_id", "subject", "description"]
    })
}

/// JSON schema equivalent of the TypeBox `TeamTaskListParams` definition.
pub fn team_task_list_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "team_run_id": { "description": "Team run id.", "type": "string" },
            "status": task_status_schema(),
            "owner": { "description": "Filter by owning member.", "type": "string" }
        },
        "required": ["team_run_id"]
    })
}

/// JSON schema equivalent of the TypeBox `TeamTaskGetParams` definition.
pub fn team_task_get_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "team_run_id": { "description": "Team run id (returned by team_create).", "type": "string" },
            "task_id": { "description": "Team tasklist task id (not a child st_... id).", "type": "string" }
        },
        "required": ["team_run_id", "task_id"]
    })
}

/// JSON schema equivalent of the TypeBox `TeamTaskUpdateParams` definition.
pub fn team_task_update_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "team_run_id": { "description": "Team run id (returned by team_create).", "type": "string" },
            "task_id": { "description": "Team tasklist task id (not a child st_... id).", "type": "string" },
            "status": task_status_schema(),
            "owner": { "description": "Owning member (defaults to the lead).", "type": "string" }
        },
        "required": ["team_run_id", "task_id", "status"]
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamTaskCreateInput {
    pub team_run_id: String,
    pub subject: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamTaskListInput {
    pub team_run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamTaskGetInput {
    pub team_run_id: String,
    pub task_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamTaskUpdateInput {
    pub team_run_id: String,
    pub task_id: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamTaskCreateDetails {
    Created { task: Task },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamTaskListDetails {
    List { tasks: Vec<Task> },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamTaskGetDetails {
    Task { task: Box<Task> },
    NotFound { task_id: String },
}

impl TeamTaskGetDetails {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Task { .. } => "task",
            Self::NotFound { .. } => "not_found",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamTaskUpdateDetails {
    Updated { task: Box<Task> },
    AlreadyClaimed { task_id: String, reason: String },
    BlockedBy { task_id: String, reason: String },
    InvalidTransition { task_id: String, reason: String },
    CrossOwner { task_id: String, reason: String },
}

impl TeamTaskUpdateDetails {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Updated { .. } => "updated",
            Self::AlreadyClaimed { .. } => "already_claimed",
            Self::BlockedBy { .. } => "blocked_by",
            Self::InvalidTransition { .. } => "invalid_transition",
            Self::CrossOwner { .. } => "cross_owner",
        }
    }
}

fn is_js_whitespace(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// `value.replace(/\s+/g, " ").trim()`, capped at `max` UTF-16 units plus `...`.
pub fn collapse_echo(value: &str, max: usize) -> String {
    let collapsed = value
        .split(is_js_whitespace)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.encode_utf16().count() <= max {
        return collapsed;
    }
    let mut units = 0usize;
    let mut head = String::new();
    for c in collapsed.chars() {
        let width = c.len_utf16();
        if units + width > max {
            break;
        }
        units += width;
        head.push(c);
    }
    format!("{head}...")
}

fn echo(value: &str) -> String {
    collapse_echo(value, ECHO_TEXT_MAX)
}

fn task_status_text(status: &TaskStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Equivalent of the TS `isMissingStateError` check on a service failure (ENOENT signature).
pub fn is_missing_state_service_error(error: &TeamToolServiceError) -> bool {
    error.name == "ENOENT" || error.message.contains("ENOENT") || error.message.contains("No such file or directory")
}

pub fn run_team_task_create(
    service: &dyn TeamToolsService,
    params: &TeamTaskCreateInput,
) -> Result<AgentToolResult<TeamTaskCreateDetails>, TeamToolServiceError> {
    let task = service.create_task(
        &params.team_run_id,
        &CreateTeamTaskServiceInput {
            subject: params.subject.clone(),
            description: params.description.clone(),
            status: TaskStatus::Pending,
            owner: None,
            blocked_by: params.blocked_by.clone(),
        },
    )?;
    let blockers = if task.blocked_by.is_empty() {
        String::new()
    } else {
        format!(", blocked by: {}", echo(&task.blocked_by.join(", ")))
    };
    let text = format!(
        "Created task {}: '{}' (status: {}{blockers}).",
        task.id,
        echo(&task.subject),
        task_status_text(&task.status)
    );
    Ok(tool_result(&text, TeamTaskCreateDetails::Created { task }))
}

pub fn run_team_task_list(
    service: &dyn TeamToolsService,
    params: &TeamTaskListInput,
) -> Result<AgentToolResult<TeamTaskListDetails>, TeamToolServiceError> {
    let filter = TeamTaskListFilter {
        status: params.status,
        owner: params.owner.clone(),
    };
    let tasks = service.list_tasks(&params.team_run_id, Some(&filter))?;
    let mut lines = vec![format!("{} task(s).", tasks.len())];
    lines.extend(tasks.iter().map(|task| {
        let owner = task
            .owner
            .as_deref()
            .map(|owner| format!(" owner:{}", echo(owner)))
            .unwrap_or_default();
        let blockers = if task.blocked_by.is_empty() {
            String::new()
        } else {
            format!(" (blocked by: {})", echo(&task.blocked_by.join(", ")))
        };
        format!(
            "- {} [{}]{owner} '{}'{blockers}",
            task.id,
            task_status_text(&task.status),
            echo(&task.subject)
        )
    }));
    Ok(tool_result(&lines.join("\n"), TeamTaskListDetails::List { tasks }))
}

pub fn run_team_task_get(
    service: &dyn TeamToolsService,
    params: &TeamTaskGetInput,
) -> Result<AgentToolResult<TeamTaskGetDetails>, TeamToolServiceError> {
    let task = match service.get_task(&params.team_run_id, &params.task_id) {
        Ok(task) => task,
        Err(error) if is_missing_state_service_error(&error) => {
            return Ok(tool_result(
                &format!("No task '{}'.", params.task_id),
                TeamTaskGetDetails::NotFound {
                    task_id: params.task_id.clone(),
                },
            ));
        }
        Err(error) => return Err(error),
    };
    let mut lines = vec![
        format!("Task {}: {}.", task.id, task_status_text(&task.status)),
        format!("subject: {}", echo(&task.subject)),
    ];
    if let Some(owner) = task.owner.as_deref() {
        lines.push(format!("owner: {}", echo(owner)));
    }
    lines.push(format!(
        "description: {}",
        collapse_echo(&task.description, ECHO_DESCRIPTION_MAX)
    ));
    if !task.blocks.is_empty() {
        lines.push(format!("blocks: {}", echo(&task.blocks.join(", "))));
    }
    if !task.blocked_by.is_empty() {
        lines.push(format!("blocked by: {}", echo(&task.blocked_by.join(", "))));
    }
    Ok(tool_result(&lines.join("\n"), TeamTaskGetDetails::Task { task: Box::new(task) }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateFailureKind {
    AlreadyClaimed,
    BlockedBy,
    InvalidTransition,
    CrossOwner,
}

fn classify_update_failure(name: &str) -> Option<UpdateFailureKind> {
    match name {
        "TeamTaskAlreadyClaimedError" | "TaskAlreadyClaimedError" => Some(UpdateFailureKind::AlreadyClaimed),
        "TeamTaskBlockedByError" | "TaskBlockedByError" => Some(UpdateFailureKind::BlockedBy),
        "TeamTaskInvalidTransitionError" | "TaskInvalidTransitionError" | "InvalidTaskTransitionError" => {
            Some(UpdateFailureKind::InvalidTransition)
        }
        "TeamTaskCrossOwnerUpdateError" | "TaskCrossOwnerUpdateError" => Some(UpdateFailureKind::CrossOwner),
        _ => None,
    }
}

pub fn run_team_task_update(
    service: &dyn TeamToolsService,
    params: &TeamTaskUpdateInput,
) -> Result<AgentToolResult<TeamTaskUpdateDetails>, TeamToolServiceError> {
    let outcome = service.update_task(&UpdateTeamTaskServiceInput {
        team_run_id: params.team_run_id.clone(),
        task_id: params.task_id.clone(),
        status: params.status,
        owner: params.owner.clone(),
    });
    match outcome {
        Ok(task) => {
            let owner = task
                .owner
                .as_deref()
                .map(|owner| format!(" (owner: {})", echo(owner)))
                .unwrap_or_default();
            let text = format!(
                "Updated task {} to {}{owner}: '{}'.",
                task.id,
                task_status_text(&task.status),
                echo(&task.subject)
            );
            Ok(tool_result(&text, TeamTaskUpdateDetails::Updated { task: Box::new(task) }))
        }
        Err(error) => {
            let Some(kind) = classify_update_failure(&error.name) else {
                return Err(error);
            };
            let reason = error.message;
            let task_id = params.task_id.clone();
            let details = match kind {
                UpdateFailureKind::AlreadyClaimed => TeamTaskUpdateDetails::AlreadyClaimed {
                    task_id,
                    reason: reason.clone(),
                },
                UpdateFailureKind::BlockedBy => TeamTaskUpdateDetails::BlockedBy {
                    task_id,
                    reason: reason.clone(),
                },
                UpdateFailureKind::InvalidTransition => TeamTaskUpdateDetails::InvalidTransition {
                    task_id,
                    reason: reason.clone(),
                },
                UpdateFailureKind::CrossOwner => TeamTaskUpdateDetails::CrossOwner {
                    task_id,
                    reason: reason.clone(),
                },
            };
            Ok(tool_result(&reason, details))
        }
    }
}

pub fn create_team_task_create_tool(deps: &TeamToolDeps) -> TeamTool<TeamTaskCreateInput, TeamTaskCreateDetails> {
    TeamTool {
        name: TASK_CREATE_TOOL_NAME,
        label: TASK_CREATE_TOOL_LABEL,
        description: TASK_CREATE_DESCRIPTION,
        parameters: team_task_create_params_schema(),
        service: deps.service.clone(),
        run: run_team_task_create,
    }
}

pub fn create_team_task_list_tool(deps: &TeamToolDeps) -> TeamTool<TeamTaskListInput, TeamTaskListDetails> {
    TeamTool {
        name: TASK_LIST_TOOL_NAME,
        label: TASK_LIST_TOOL_LABEL,
        description: TASK_LIST_DESCRIPTION,
        parameters: team_task_list_params_schema(),
        service: deps.service.clone(),
        run: run_team_task_list,
    }
}

pub fn create_team_task_get_tool(deps: &TeamToolDeps) -> TeamTool<TeamTaskGetInput, TeamTaskGetDetails> {
    TeamTool {
        name: TASK_GET_TOOL_NAME,
        label: TASK_GET_TOOL_LABEL,
        description: TASK_GET_DESCRIPTION,
        parameters: team_task_get_params_schema(),
        service: deps.service.clone(),
        run: run_team_task_get,
    }
}

pub fn create_team_task_update_tool(deps: &TeamToolDeps) -> TeamTool<TeamTaskUpdateInput, TeamTaskUpdateDetails> {
    TeamTool {
        name: TASK_UPDATE_TOOL_NAME,
        label: TASK_UPDATE_TOOL_LABEL,
        description: TASK_UPDATE_DESCRIPTION,
        parameters: team_task_update_params_schema(),
        service: deps.service.clone(),
        run: run_team_task_update,
    }
}
