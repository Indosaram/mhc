//! Shared types for the senpi team runtime: errors, manager/destruction ports, deps and results.

use std::io;
use std::path::Path;
use std::sync::Arc;

use team_core::types::RuntimeState;

use crate::lifecycle::port::DestroyCause;
use crate::manager::execution_mode::ExecutionMode;
use crate::state::{ResidencyState, ResolvedModelRecord, TaskStatus};
use crate::store::StateDirConfig;
use crate::team::member_map::MemberTaskMap;
use crate::team::member_projection::{MemberStatusPort, MemberTaskStatusView, ResidentSessionRef, RuntimeMemberStatus};
use crate::team::runtime_config::TeamTaskBounds;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenpiTeamRuntimeErrorCode {
    BoundsExceeded,
    MemberStartRejected,
    CreateDeadlineExceeded,
    InvalidDeleteState,
    SidecarWriteFailed,
}

impl SenpiTeamRuntimeErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BoundsExceeded => "bounds_exceeded",
            Self::MemberStartRejected => "member_start_rejected",
            Self::CreateDeadlineExceeded => "create_deadline_exceeded",
            Self::InvalidDeleteState => "invalid_delete_state",
            Self::SidecarWriteFailed => "sidecar_write_failed",
        }
    }
}

/// Raised by the team runtime for lifecycle failures distinct from spec normalization
/// (`SenpiTeamSpecError`): bounds rejection before any spawn, a member start rejected by the manager,
/// a create-deadline breach, and an illegal delete transition. Carries the team identifier in play.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SenpiTeamRuntimeError {
    pub message: String,
    pub code: SenpiTeamRuntimeErrorCode,
    pub team_ref: String,
}

impl SenpiTeamRuntimeError {
    pub fn new(message: impl Into<String>, code: SenpiTeamRuntimeErrorCode, team_ref: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code,
            team_ref: team_ref.into(),
        }
    }

    pub fn name(&self) -> &'static str {
        "SenpiTeamRuntimeError"
    }
}

/// The start spec the team runtime hands the manager for one member.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamMemberStartSpec {
    pub name: Option<String>,
    pub description: Option<String>,
    pub prompt: String,
    pub parent_session_id: String,
    pub root_session_id: Option<String>,
    pub depth: u32,
    pub execution_mode: Option<ExecutionMode>,
    pub model: Option<String>,
    pub category: Option<String>,
    pub subagent_type: Option<String>,
}

/// The narrow task-record view the team runtime reads through the manager port.
#[derive(Debug, Clone)]
pub struct TeamMemberTaskRecord {
    pub task_id: String,
    pub status: TaskStatus,
    pub residency_state: ResidencyState,
    pub created_at: String,
    pub updated_at: String,
    pub parent_session_id: String,
    pub root_session_id: String,
    pub depth: u32,
    pub execution_mode: ExecutionMode,
    pub model: String,
    pub child_session_id: Option<String>,
    pub resolved_model: Option<ResolvedModelRecord>,
    pub name: Option<String>,
    pub category: Option<String>,
    pub agent_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TeamStartedMember {
    pub task_id: String,
    pub status: TaskStatus,
    pub name: String,
    pub resolved_model: Option<ResolvedModelRecord>,
}

#[derive(Debug, Clone)]
pub enum TeamStartResult {
    Started(TeamStartedMember),
    /// Any non-started manager result (`kind` is the manager result kind, e.g. `error`).
    Rejected { kind: String, reason: String },
}

impl TeamStartResult {
    pub fn kind(&self) -> &str {
        match self {
            Self::Started(_) => "started",
            Self::Rejected { kind, .. } => kind,
        }
    }
}

#[derive(Debug, Clone)]
pub enum TeamCancelOutcome {
    NotFound {
        reason: String,
    },
    Noop {
        task_id: String,
        status: TaskStatus,
        reason: String,
    },
    Cancelled {
        task_id: String,
        previous_status: TaskStatus,
    },
}

impl TeamCancelOutcome {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not_found",
            Self::Noop { .. } => "noop",
            Self::Cancelled { .. } => "cancelled",
        }
    }
}

pub trait TeamMemberReadPort {
    fn get(&self, task_id: &str) -> Option<TeamMemberTaskRecord>;
}

pub trait TeamMemberCancelPort: TeamMemberReadPort {
    fn cancel_task(&self, id_or_name: &str, reason: Option<&str>) -> TeamCancelOutcome;
}

// The TaskManager surface the team runtime spawns and cancels members through; kept narrow so the
// runtime never reaches past start/cancel/read. A thrown start surfaces as `Err(message)`.
pub trait TeamRuntimeManagerPort: TeamMemberCancelPort {
    fn start(&self, spec: &TeamMemberStartSpec) -> Result<TeamStartResult, String>;
    fn get_resident_handle(&self, task_id: &str) -> Option<ResidentSessionRef>;
}

impl<T: TeamRuntimeManagerPort + ?Sized> MemberStatusPort for T {
    fn get(&self, task_id: &str) -> Option<MemberTaskStatusView> {
        TeamMemberReadPort::get(self, task_id).map(|record| MemberTaskStatusView {
            status: record.status,
            child_session_id: record.child_session_id,
        })
    }

    fn get_resident_handle(&self, task_id: &str) -> Option<ResidentSessionRef> {
        TeamRuntimeManagerPort::get_resident_handle(self, task_id)
    }
}

/// Lifecycle single-writer destruction port.
pub trait TeamMemberDestructionPort {
    fn destroy_resident_task(&self, task_id: &str, cause: DestroyCause) -> Result<(), String>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamMemberExtensionConfig {
    pub entry_path: String,
    pub inherited_extensions: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnMemberExtensionConfig {
    pub entry_path: String,
    pub inherited_extensions: Option<Vec<String>>,
    pub team_config: String,
}

pub type TeamNowFn = Arc<dyn Fn() -> i64 + Send + Sync>;
pub type WriteMemberMapFn = Arc<dyn Fn(&Path, &MemberTaskMap) -> io::Result<()> + Send + Sync>;

pub struct CreateTeamDeps {
    pub manager: Arc<dyn TeamRuntimeManagerPort>,
    pub state_dir: StateDirConfig,
    pub team_bounds: TeamTaskBounds,
    pub lead_session_id: String,
    pub spawn_depth: u32,
    pub now: Option<TeamNowFn>,
    pub member_extension: Option<TeamMemberExtensionConfig>,
    // Injectable member-sidecar writer (defaults to the atomic write_member_task_map). Present so
    // tests can force the pre-activation write to fail and exercise the create rollback.
    pub write_member_map: Option<WriteMemberMapFn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreatedMemberRole {
    Category { category: String },
    SubagentType { subagent_type: String },
}

impl CreatedMemberRole {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Category { .. } => "category",
            Self::SubagentType { .. } => "subagent_type",
        }
    }
}

// Caller-facing view of one spawned member: identity and live status from the runtime state, the
// role from the spec, the resolved model captured at spawn, and a bounded prompt excerpt.
#[derive(Debug, Clone)]
pub struct CreatedMemberInfo {
    pub name: String,
    pub task_id: String,
    pub status: RuntimeMemberStatus,
    pub role: CreatedMemberRole,
    pub model: Option<ResolvedModelRecord>,
    pub prompt_excerpt: Option<String>,
    pub task_summary: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateTeamResult {
    pub runtime_state: RuntimeState,
    pub member_task_ids: MemberTaskMap,
    pub members: Vec<CreatedMemberInfo>,
}

pub struct DeleteTeamDeps {
    pub manager: Arc<dyn TeamMemberCancelPort>,
    // Lifecycle single-writer destruction port. Team deletion routes terminal-resident members
    // through it directly because terminal `cancel_task` is an intentional noop (completed residents
    // stay revivable outside deletion).
    pub destruction: Arc<dyn TeamMemberDestructionPort>,
    pub state_dir: StateDirConfig,
    pub team_bounds: TeamTaskBounds,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteTeamResult {
    pub team_run_id: String,
    pub cancelled_task_ids: Vec<String>,
}
