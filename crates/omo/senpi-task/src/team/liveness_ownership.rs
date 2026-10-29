//! Resolves whether a task record is a team member owned by the current lead session.

use std::sync::{Arc, LazyLock};

use regex::Regex;
use team_core::TeamModeConfig;
use team_core::team_state_store::load_runtime_state;
use team_core::types::RuntimeState;

use crate::store::StateDirConfig;
use crate::team::runtime_config::{TeamTaskBounds, to_team_core_config};
use crate::team::storage::team_storage_base_dir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamMemberTaskIdentity {
    pub team_run_id: String,
    pub member_name: String,
}

pub type RuntimeStateLoader = Arc<dyn Fn(&str, &TeamModeConfig) -> team_core::Result<RuntimeState> + Send + Sync>;

pub struct TeamMemberOwnershipDeps {
    pub state_dir: StateDirConfig,
    pub team_bounds: TeamTaskBounds,
    pub load_runtime_state: Option<RuntimeStateLoader>,
}

static TEAM_MEMBER_NAME_PATTERN: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)^team:([0-9a-f-]{36}):([a-z0-9-]+)$").ok());

/// Parses a `team:<teamRunId>:<memberName>` task name into its team member identity.
pub fn parse_team_member_task_identity(record_name: Option<&str>) -> Option<TeamMemberTaskIdentity> {
    let pattern = TEAM_MEMBER_NAME_PATTERN.as_ref()?;
    let captures = pattern.captures(record_name.unwrap_or(""))?;
    let team_run_id = captures.get(1)?.as_str().to_string();
    let member_name = captures.get(2)?.as_str().to_string();
    Some(TeamMemberTaskIdentity {
        team_run_id,
        member_name,
    })
}

pub fn is_owned_team_member_task(
    record_name: Option<&str>,
    current_session_id: Option<&str>,
    deps: &TeamMemberOwnershipDeps,
) -> bool {
    let Some(current_session_id) = current_session_id.filter(|id| !id.is_empty()) else {
        return false;
    };
    let Some(identity) = parse_team_member_task_identity(record_name) else {
        return false;
    };
    let base_dir = team_storage_base_dir(&deps.state_dir);
    let Ok(config) = to_team_core_config(&deps.team_bounds, &base_dir.to_string_lossy()) else {
        return false;
    };
    let loaded = match &deps.load_runtime_state {
        Some(loader) => loader(&identity.team_run_id, &config),
        None => load_runtime_state(&identity.team_run_id, &config),
    };
    let Ok(runtime_state) = loaded else {
        return false;
    };
    runtime_state.lead_session_id.as_deref() == Some(current_session_id)
        && runtime_state
            .members
            .iter()
            .any(|member| member.name == identity.member_name)
}
