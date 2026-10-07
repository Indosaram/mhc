//! Port of `tools/team/query.ts`: the read-only team status/listing tools.
//!
//! `team_status` projects one run through `aggregateStatus`; `team_list` joins the declared team
//! specs with the active runs, keeping a declared-only team as `not-started` and appending an
//! active team that has no declared spec. Both drive the same `TeamToolsService` seam as the
//! mutating lead tools, so the whole family registers through [`crate::tools::team::index`].

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools::control::tool_result::{AgentToolResult, tool_result};
use crate::tools::team::index::TeamTool;
use crate::tools::team::types::{
    ActiveTeamSummary, DiscoveredTeamSpec, TeamListEntry, TeamListScope, TeamStatus, TeamToolDeps,
    TeamToolServiceError, TeamToolsService,
};

pub const TEAM_STATUS_TOOL_NAME: &str = "team_status";
pub const TEAM_STATUS_TOOL_LABEL: &str = "Team Status";
pub const TEAM_LIST_TOOL_NAME: &str = "team_list";
pub const TEAM_LIST_TOOL_LABEL: &str = "Team List";

pub const STATUS_DESCRIPTION: &str = "Return full status for a team run.";
pub const LIST_DESCRIPTION: &str = "List declared and active teams.";

/// JSON schema equivalent of the TypeBox `TeamStatusParams` definition.
pub fn team_status_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "teamRunId": { "description": "Team run ID", "type": "string" }
        },
        "required": ["teamRunId"]
    })
}

/// JSON schema equivalent of the TypeBox `TeamListParams` definition.
pub fn team_list_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "scope": {
                "description": "Team scope filter",
                "anyOf": [
                    { "const": "user", "type": "string" },
                    { "const": "project", "type": "string" },
                    { "const": "all", "type": "string" }
                ]
            }
        }
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamStatusInput {
    pub team_run_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TeamListInput {
    #[serde(default)]
    pub scope: Option<TeamListScope>,
}

pub fn run_team_status(
    service: &dyn TeamToolsService,
    params: &TeamStatusInput,
) -> Result<AgentToolResult<TeamStatus>, TeamToolServiceError> {
    let status = service.aggregate_status(&params.team_run_id)?;
    let text = format!(
        "Team '{}' ({}) is {}.",
        status.team_name, status.team_run_id, status.status
    );
    Ok(tool_result(&text, status))
}

pub fn run_team_list(
    service: &dyn TeamToolsService,
    params: &TeamListInput,
) -> Result<AgentToolResult<Vec<TeamListEntry>>, TeamToolServiceError> {
    let scope = params.scope.unwrap_or(TeamListScope::All);
    let project_root = service.project_root();
    run_team_list_in(service, scope, &project_root)
}

pub fn run_team_list_in(
    service: &dyn TeamToolsService,
    scope: TeamListScope,
    project_root: &Path,
) -> Result<AgentToolResult<Vec<TeamListEntry>>, TeamToolServiceError> {
    let declared = service.discover_team_specs(project_root)?;
    let filtered: Vec<DiscoveredTeamSpec> = match scope.spec_scope() {
        Some(wanted) => declared
            .into_iter()
            .filter(|spec| spec.scope == wanted)
            .collect(),
        None => declared,
    };

    let mut declared_member_counts: BTreeMap<String, usize> = BTreeMap::new();
    for spec in &filtered {
        let count = service.load_team_spec_member_count(&spec.name, project_root)?;
        declared_member_counts.insert(spec.name.clone(), count);
    }

    let active = service.list_teams()?;
    let active_by_name: BTreeMap<&str, &ActiveTeamSummary> =
        active.iter().map(|team| (team.team_name.as_str(), team)).collect();

    let mut entries: Vec<TeamListEntry> = Vec::new();
    for spec in &filtered {
        let active_team = active_by_name.get(spec.name.as_str());
        let declared_count = declared_member_counts.get(&spec.name).copied().unwrap_or(0);
        entries.push(TeamListEntry {
            name: spec.name.clone(),
            scope: spec.scope,
            status: active_team.map_or_else(|| "not-started".to_string(), |team| team.status.clone()),
            team_run_id: active_team.map(|team| team.team_run_id.clone()),
            member_count: active_team.map_or(declared_count, |team| team.member_count),
        });
    }

    for team in &active {
        if declared_member_counts.contains_key(&team.team_name) {
            continue;
        }
        entries.push(TeamListEntry {
            name: team.team_name.clone(),
            scope: team.scope,
            status: team.status.clone(),
            team_run_id: Some(team.team_run_id.clone()),
            member_count: team.member_count,
        });
    }

    let text = format!("{} team(s).", entries.len());
    Ok(tool_result(&text, entries))
}

pub fn create_team_status_tool(deps: &TeamToolDeps) -> TeamTool<TeamStatusInput, TeamStatus> {
    TeamTool {
        name: TEAM_STATUS_TOOL_NAME,
        label: TEAM_STATUS_TOOL_LABEL,
        description: STATUS_DESCRIPTION,
        parameters: team_status_params_schema(),
        service: deps.service.clone(),
        run: run_team_status,
    }
}

pub fn create_team_list_tool(deps: &TeamToolDeps) -> TeamTool<TeamListInput, Vec<TeamListEntry>> {
    TeamTool {
        name: TEAM_LIST_TOOL_NAME,
        label: TEAM_LIST_TOOL_LABEL,
        description: LIST_DESCRIPTION,
        parameters: team_list_params_schema(),
        service: deps.service.clone(),
        run: run_team_list,
    }
}
