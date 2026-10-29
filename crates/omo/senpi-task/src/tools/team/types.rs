//! Service seam and value types shared by the lead team tools.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use team_core::types::{MemberStatus, RuntimeBounds, RuntimeState, ShutdownRequest, Task, TaskStatus};

use crate::team::messaging::types::{SendTeamMessageInput, SendTeamMessageResult};
use crate::team::runtime_types::{CreateTeamResult, DeleteTeamResult};

/// Provenance scope of an active team run: a project spec file or a user (`omo.json`) spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ActiveTeamScope {
    Project,
    User,
}

impl ActiveTeamScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
        }
    }
}

impl std::fmt::Display for ActiveTeamScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveTeamSummary {
    pub team_run_id: String,
    pub team_name: String,
    pub status: String,
    pub member_count: usize,
    pub scope: ActiveTeamScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lead_session_id: Option<String>,
}

/// The team-core task status vocabulary (`Task["status"]`).
pub type TeamTaskStatus = TaskStatus;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTeamToolInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_spec: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteTeamToolInput {
    pub team_run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTeamTaskServiceInput {
    pub subject: String,
    pub description: String,
    pub status: TeamTaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTeamTaskServiceInput {
    pub team_run_id: String,
    pub task_id: String,
    pub status: TeamTaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamTaskListFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<TeamTaskStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// Failure surfaced by a team-tools service call (the rejected promise of the TS service).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct TeamToolServiceError {
    pub name: String,
    pub message: String,
    /// The typed error's `code` (`SenpiTeamSpecError` / `SenpiTeamRuntimeError` / `SenpiShutdownError`).
    pub code: Option<String>,
}

impl TeamToolServiceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            name: "Error".to_string(),
            message: message.into(),
            code: None,
        }
    }

    pub fn with_name(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            message: message.into(),
            code: None,
        }
    }

    pub fn with_code(name: impl Into<String>, message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            message: message.into(),
            code: Some(code.into()),
        }
    }
}

pub type TeamServiceResult<T> = Result<T, TeamToolServiceError>;

/// The service the lead team tools drive. Async TS methods become synchronous calls.
pub trait TeamToolsService: Send + Sync {
    fn create_team(&self, input: &CreateTeamToolInput) -> TeamServiceResult<CreateTeamResult>;
    fn delete_team(&self, input: &DeleteTeamToolInput) -> TeamServiceResult<DeleteTeamResult>;
    fn send_message(
        &self,
        team_run_id: &str,
        input: &SendTeamMessageInput,
    ) -> TeamServiceResult<SendTeamMessageResult>;
    fn status(&self, team_run_id: &str) -> TeamServiceResult<RuntimeState>;
    fn list_teams(&self) -> TeamServiceResult<Vec<ActiveTeamSummary>>;
    fn create_task(&self, team_run_id: &str, input: &CreateTeamTaskServiceInput) -> TeamServiceResult<Task>;
    fn list_tasks(&self, team_run_id: &str, filter: Option<&TeamTaskListFilter>) -> TeamServiceResult<Vec<Task>>;
    fn update_task(&self, input: &UpdateTeamTaskServiceInput) -> TeamServiceResult<Task>;
    fn get_task(&self, team_run_id: &str, task_id: &str) -> TeamServiceResult<Task>;
    fn request_shutdown(&self, team_run_id: &str, member: &str) -> TeamServiceResult<RuntimeState>;
    fn approve_shutdown(&self, team_run_id: &str, member: &str) -> TeamServiceResult<RuntimeState>;
    fn reject_shutdown(&self, team_run_id: &str, member: &str, reason: &str) -> TeamServiceResult<RuntimeState>;
    /// `aggregateStatus`: the full status projection of one team run (`team_status`).
    fn aggregate_status(&self, team_run_id: &str) -> TeamServiceResult<TeamStatus>;
    /// `discoverTeamSpecs`: every declared spec under `project_root/.omo/teams` and the user dir.
    fn discover_team_specs(&self, project_root: &Path) -> TeamServiceResult<Vec<DiscoveredTeamSpec>>;
    /// `loadTeamSpec(...).members.length`: the declared member count of one named spec.
    fn load_team_spec_member_count(&self, name: &str, project_root: &Path) -> TeamServiceResult<usize>;
    fn project_root(&self) -> std::path::PathBuf {
        std::env::current_dir().unwrap_or_default()
    }
}

#[derive(Clone)]
pub struct TeamToolDeps {
    pub service: Arc<dyn TeamToolsService>,
}

pub type LeadTeamToolDeps = TeamToolDeps;

/// The `scope` argument of `team_list` (`TeamListScope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TeamListScope {
    User,
    Project,
    All,
}

impl TeamListScope {
    /// The spec scope this filter selects, or `None` for `all`.
    #[must_use]
    pub fn spec_scope(self) -> Option<ActiveTeamScope> {
        match self {
            Self::User => Some(ActiveTeamScope::User),
            Self::Project => Some(ActiveTeamScope::Project),
            Self::All => None,
        }
    }
}

/// One entry of the `team_list` result (`TeamListEntry`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamListEntry {
    pub name: String,
    pub scope: ActiveTeamScope,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_run_id: Option<String>,
    pub member_count: usize,
}

/// A declared team spec as `discoverTeamSpecs` returns it (`TeamSpecEntry`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredTeamSpec {
    pub name: String,
    pub scope: ActiveTeamScope,
    pub path: String,
}

/// One member row of the `team_status` aggregate (`TeamStatus["members"][number]`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamStatusMember {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub status: MemberStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    pub unread_messages: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

/// The tasklist counts of `team_status` (`TeamStatus["tasks"]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamStatusTasks {
    pub pending: usize,
    pub claimed: usize,
    pub in_progress: usize,
    pub completed: usize,
    pub deleted: usize,
    pub total: usize,
}

/// The concurrency block of `team_status` (`TeamStatus["concurrency"]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamStatusConcurrency {
    pub running_on_same_model: usize,
    pub queued_on_same_model: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_run_id_specific: Option<usize>,
}

/// The `aggregateStatus` projection of a team run (`TeamStatus`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamStatus {
    pub team_name: String,
    pub team_run_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lead_session_id: Option<String>,
    pub created_at: i64,
    pub members: Vec<TeamStatusMember>,
    pub tasks: TeamStatusTasks,
    pub shutdown_requests: Vec<ShutdownRequest>,
    pub concurrency: TeamStatusConcurrency,
    pub bounds: RuntimeBounds,
    pub stale_locks: Vec<String>,
}
