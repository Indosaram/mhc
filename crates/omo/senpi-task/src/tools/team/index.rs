//! Port of `tools/team/index.ts`.
//!
//! The TS barrel re-exports are not reproduced (callers import from the concrete modules); this
//! module holds the minimal `ToolDefinition` model shared by the lead team tools and the
//! `buildLeadTeamTools` registrar.

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::tools::control::tool_result::AgentToolResult;
use crate::tools::team::lifecycle::{
    TeamCreateDetails, TeamCreateInput, TeamDeleteDetails, TeamDeleteInput, create_team_create_tool,
    create_team_delete_tool,
};
use crate::tools::team::tasks::{
    TeamTaskCreateDetails, TeamTaskCreateInput, TeamTaskGetDetails, TeamTaskGetInput, TeamTaskListDetails,
    TeamTaskListInput, TeamTaskUpdateDetails, TeamTaskUpdateInput, create_team_task_create_tool,
    create_team_task_get_tool, create_team_task_list_tool, create_team_task_update_tool,
};
use crate::tools::team::query::{
    TeamListInput, TeamStatusInput, create_team_list_tool, create_team_status_tool,
};
use crate::tools::team::types::{
    LeadTeamToolDeps, TeamListEntry, TeamStatus, TeamToolServiceError, TeamToolsService,
};

/// Runner signature shared by every lead team tool (the TS `execute` body).
pub type TeamRunFn<P, D> = fn(&dyn TeamToolsService, &P) -> Result<AgentToolResult<D>, TeamToolServiceError>;

/// Minimal model of a senpi `ToolDefinition` bound to the team tools service.
pub struct TeamTool<P, D> {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    pub service: Arc<dyn TeamToolsService>,
    pub run: TeamRunFn<P, D>,
}

/// Failures of the JSON-level tool execution seam.
#[derive(Debug, thiserror::Error)]
pub enum LeadTeamToolError {
    #[error("{0}")]
    InvalidParams(String),
    #[error("{0}")]
    Serialization(String),
    #[error(transparent)]
    Service(#[from] TeamToolServiceError),
}

impl<P, D> TeamTool<P, D> {
    pub fn execute(&self, _tool_call_id: &str, params: &P) -> Result<AgentToolResult<D>, TeamToolServiceError> {
        (self.run)(self.service.as_ref(), params)
    }
}

impl<P: DeserializeOwned, D: Serialize> TeamTool<P, D> {
    /// Executes the tool from raw JSON params and returns the JSON-encoded tool result.
    pub fn execute_json(&self, tool_call_id: &str, params: &Value) -> Result<Value, LeadTeamToolError> {
        let parsed: P =
            serde_json::from_value(params.clone()).map_err(|error| LeadTeamToolError::InvalidParams(error.to_string()))?;
        let result = self.execute(tool_call_id, &parsed)?;
        serde_json::to_value(&result).map_err(|error| LeadTeamToolError::Serialization(error.to_string()))
    }
}

/// One registered lead team tool (the heterogeneous `ToolDefinition[]` of the TS registrar).
pub enum LeadTeamTool {
    Create(TeamTool<TeamCreateInput, TeamCreateDetails>),
    Delete(TeamTool<TeamDeleteInput, TeamDeleteDetails>),
    TaskCreate(TeamTool<TeamTaskCreateInput, TeamTaskCreateDetails>),
    TaskGet(TeamTool<TeamTaskGetInput, TeamTaskGetDetails>),
    TaskList(TeamTool<TeamTaskListInput, TeamTaskListDetails>),
    TaskUpdate(TeamTool<TeamTaskUpdateInput, TeamTaskUpdateDetails>),
    Status(TeamTool<TeamStatusInput, TeamStatus>),
    List(TeamTool<TeamListInput, Vec<TeamListEntry>>),
}

macro_rules! with_tool {
    ($self:expr, $tool:ident => $body:expr) => {
        match $self {
            LeadTeamTool::Create($tool) => $body,
            LeadTeamTool::Delete($tool) => $body,
            LeadTeamTool::TaskCreate($tool) => $body,
            LeadTeamTool::TaskGet($tool) => $body,
            LeadTeamTool::TaskList($tool) => $body,
            LeadTeamTool::TaskUpdate($tool) => $body,
            LeadTeamTool::Status($tool) => $body,
            LeadTeamTool::List($tool) => $body,
        }
    };
}

impl LeadTeamTool {
    pub fn name(&self) -> &'static str {
        with_tool!(self, tool => tool.name)
    }

    pub fn label(&self) -> &'static str {
        with_tool!(self, tool => tool.label)
    }

    pub fn description(&self) -> &'static str {
        with_tool!(self, tool => tool.description)
    }

    pub fn parameters(&self) -> &Value {
        with_tool!(self, tool => &tool.parameters)
    }

    pub fn execute_json(&self, tool_call_id: &str, params: &Value) -> Result<Value, LeadTeamToolError> {
        with_tool!(self, tool => tool.execute_json(tool_call_id, params))
    }
}

pub fn build_lead_team_tools(deps: &LeadTeamToolDeps) -> Vec<LeadTeamTool> {
    vec![
        LeadTeamTool::Create(create_team_create_tool(deps)),
        LeadTeamTool::Delete(create_team_delete_tool(deps)),
        LeadTeamTool::TaskCreate(create_team_task_create_tool(deps)),
        LeadTeamTool::TaskGet(create_team_task_get_tool(deps)),
        LeadTeamTool::TaskList(create_team_task_list_tool(deps)),
        LeadTeamTool::TaskUpdate(create_team_task_update_tool(deps)),
        LeadTeamTool::Status(create_team_status_tool(deps)),
        LeadTeamTool::List(create_team_list_tool(deps)),
    ]
}
