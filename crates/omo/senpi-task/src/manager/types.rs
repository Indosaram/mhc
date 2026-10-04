//! Manager contracts (`manager/types.ts`). The TS promise-returning surface is synchronous here:
//! runners block until the child exists, and outcome tracking runs on a watcher thread.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::DelegateFallbackEntry;
use crate::lifecycle::DestroyCause;
use crate::manager::ManagedChildHandle;
use crate::manager::concurrency::TaskConcurrencyConfig;
use crate::manager::execution_mode::ExecutionMode;
use crate::runners::RunnerFailure;
use crate::runners::in_process::shared_tool_filter::ChildToolRef;
use crate::state::{ResolvedModelRecord, TaskRecord, TaskStatus};
use crate::steering::DestructionPort;

/// What the manager hands a runner (`ManagedStartSpec`).
#[derive(Clone, Default)]
pub struct ManagedStartSpec {
    pub task_id: String,
    pub cwd: String,
    pub state_dir: String,
    pub prompt: String,
    pub depth: u32,
    pub parent_session_id: String,
    pub root_session_id: String,
    pub model: Option<String>,
    pub requested_model: Option<ResolvedModelRecord>,
    pub fallback_models: Option<Vec<ResolvedModelRecord>>,
    pub resolved_model: Option<ResolvedModelRecord>,
    pub variant: Option<String>,
    pub agent_type: Option<String>,
    pub instructions: Option<String>,
    pub tool_allowlist: Option<Vec<String>>,
    pub tool_denylist: Option<Vec<String>>,
    pub member_scoped_tool_names: Option<Vec<String>>,
    pub member_scoped_tools: Option<Vec<ChildToolRef>>,
    pub extensions: Option<Vec<String>>,
    pub member_env: Option<BTreeMap<String, String>>,
}

/// A thrown runner value. TS narrows `unknown` with `RunnerError.is` and the team launch error name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagedRunnerError {
    #[error("{}", .0.message)]
    Runner(RunnerFailure),
    /// `TeamMemberRespawnLaunchError` with its `code`.
    #[error("team member respawn launch failed: {code}")]
    TeamRespawnLaunch { code: String },
    #[error("{0}")]
    Other(String),
}

pub type ManagedRunnerResult = Result<Arc<dyn ManagedChildHandle>, ManagedRunnerError>;

pub trait ManagedRunner: Send + Sync {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult;
    /// `None` when the runner has no `resume` (TS optional method).
    fn resume(&self, _spec: &ManagedStartSpec, _session_path: &str) -> Option<ManagedRunnerResult> {
        None
    }
}

#[derive(Clone)]
pub struct ManagedRunners {
    pub in_process: Arc<dyn ManagedRunner>,
    pub process: Arc<dyn ManagedRunner>,
}

impl ManagedRunners {
    pub fn get(&self, mode: ExecutionMode) -> &Arc<dyn ManagedRunner> {
        match mode {
            ExecutionMode::InProcess => &self.in_process,
            ExecutionMode::Process => &self.process,
        }
    }
}

/// The caller's start request (`ManagerStartSpec`).
#[derive(Clone, Default)]
pub struct ManagerStartSpec {
    pub prompt: String,
    pub task_summary: Option<String>,
    pub parent_session_id: String,
    pub root_session_id: Option<String>,
    pub depth: u32,
    pub category: Option<String>,
    pub subagent_type: Option<String>,
    pub execution_mode: Option<ExecutionMode>,
    pub model: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub cwd: Option<String>,
    pub instructions: Option<String>,
    pub allowed_subagents: Option<Vec<String>>,
    pub run_in_background: bool,
    pub member_scoped_tools: Option<Vec<ChildToolRef>>,
    pub extensions: Option<Vec<String>>,
    pub member_env: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedChildPlan {
    pub model: String,
    pub requested_model: Option<ResolvedModelRecord>,
    pub fallback_models: Option<Vec<ResolvedModelRecord>>,
    pub resolved_model: Option<ResolvedModelRecord>,
    pub variant: Option<String>,
    pub agent_execution_mode: Option<ExecutionMode>,
    pub agent_type: Option<String>,
    pub category: Option<String>,
    pub instructions: Option<String>,
    pub tool_allowlist: Option<Vec<String>>,
    pub tool_denylist: Option<Vec<String>>,
    pub prompt_append: Option<String>,
    pub allowed_subagents: Option<Vec<String>>,
    pub max_depth: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanResolutionCode {
    UnknownTarget,
    ModelUnavailable,
    CategoryDisabled,
    InvalidTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanResolutionError {
    pub code: PlanResolutionCode,
    pub message: String,
    pub available_agents: Option<Vec<String>>,
    pub available_categories: Option<Vec<String>>,
    pub category: Option<String>,
    pub attempted_chain: Option<Vec<DelegateFallbackEntry>>,
    pub missing_providers: Option<Vec<String>>,
}

impl PlanResolutionError {
    pub fn new(code: PlanResolutionCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            available_agents: None,
            available_categories: None,
            category: None,
            attempted_chain: None,
            missing_providers: None,
        }
    }
}

/// The error is boxed because `PlanResolutionError` is large (`clippy::result_large_err`).
pub type ChildPlanner = Arc<
    dyn Fn(&ManagerStartSpec) -> Result<ResolvedChildPlan, Box<PlanResolutionError>> + Send + Sync,
>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedTask {
    pub task_id: String,
    /// `running` or `pending`.
    pub status: TaskStatus,
    pub name: String,
    pub resolved_model: Option<ResolvedModelRecord>,
    pub queue_position: Option<usize>,
    pub name_warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartFailure {
    pub task_id: String,
    pub name: String,
    pub category: Option<String>,
    pub subagent_type: Option<String>,
    pub execution_mode: ExecutionMode,
    pub model: String,
    pub resolved_model: Option<ResolvedModelRecord>,
    pub run_in_background: bool,
    pub error_message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartResult {
    Started(StartedTask),
    DepthDenied {
        reason: String,
        child_depth: u32,
        max_depth: u32,
    },
    PlanUnresolved(PlanResolutionError),
    StartFailed(StartFailure),
    ResidencyDenied {
        reason: String,
    },
}

impl StartResult {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Started(_) => "started",
            Self::DepthDenied { .. } => "depth_denied",
            Self::PlanUnresolved(_) => "plan_unresolved",
            Self::StartFailed(_) => "start_failed",
            Self::ResidencyDenied { .. } => "residency_denied",
        }
    }
}

/// `OwnedStartResult`: a started result carries `reused`; the rest mirror [`StartResult`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedStartResult {
    Started {
        task: StartedTask,
        reused: bool,
    },
    OwnerConflict {
        task_id: String,
        existing_fingerprint: String,
        requested_fingerprint: String,
    },
    NotStarted(StartResult),
}

impl OwnedStartResult {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::OwnerConflict { .. } => "owner_conflict",
            Self::NotStarted(result) => result.kind(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListScope {
    ParentSession(String),
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListedTask {
    pub record: TaskRecord,
    pub queue_position: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnAdmission {
    Admitted,
    Evicted { evicted_task_id: String },
    Rejected { message: String },
}

pub type AdmitResident = Arc<dyn Fn(&str) -> SpawnAdmission + Send + Sync>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedRespawnLaunch {
    pub extensions: Option<Vec<String>>,
    pub member_env: Option<BTreeMap<String, String>>,
}

pub type TrustedRespawnLaunchResolver = Arc<
    dyn Fn(&TaskRecord) -> Result<Option<TrustedRespawnLaunch>, ManagedRunnerError> + Send + Sync,
>;

/// The RPC runner the manager respawns process children through.
pub trait RpcRespawnRunner: Send + Sync {
    fn start(&self, spec: &crate::runners::types::RpcRunnerSpec) -> ManagedRunnerResult;
}

/// The subset of `OmoTaskSettings` the manager reads.
#[derive(Debug, Clone)]
pub struct ManagerConfig {
    pub concurrency: TaskConcurrencyConfig,
    pub max_depth: u32,
    pub default_execution_mode: ExecutionMode,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            concurrency: TaskConcurrencyConfig {
                default_concurrency: Some(5),
                provider_concurrency: None,
                model_concurrency: None,
            },
            max_depth: 1,
            default_execution_mode: ExecutionMode::InProcess,
        }
    }
}

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct TaskManagerOptions {
    pub store: crate::store::TaskRecordStore,
    pub runners: ManagedRunners,
    pub planner: ChildPlanner,
    pub config: ManagerConfig,
    pub cwd: String,
    pub now: Option<Clock>,
    pub destruction: Option<Arc<dyn DestructionPort>>,
    pub admit: Option<AdmitResident>,
    /// Host/lifecycle errors are failures, not capacity denials that DAGs may retry.
    pub fallible_admit: Option<Arc<dyn Fn(&str) -> Result<SpawnAdmission, crate::host::HostError> + Send + Sync>>,
    pub trusted_respawn_launch: Option<TrustedRespawnLaunchResolver>,
    pub host_pid: Option<i64>,
    pub rpc_respawn_runner: Option<Arc<dyn RpcRespawnRunner>>,
    /// Overrides the claim saver (TS tests wrap `store.save`); defaults to the store.
    pub record_saver: Option<Arc<dyn crate::store::TaskRecordSaver + Send + Sync>>,
}

impl TaskManagerOptions {
    pub fn new(
        store: crate::store::TaskRecordStore,
        runners: ManagedRunners,
        planner: ChildPlanner,
        cwd: impl Into<String>,
    ) -> Self {
        Self {
            store,
            runners,
            planner,
            config: ManagerConfig::default(),
            cwd: cwd.into(),
            now: None,
            destruction: None,
            admit: None,
            fallible_admit: None,
            trusted_respawn_launch: None,
            host_pid: None,
            rpc_respawn_runner: None,
            record_saver: None,
        }
    }
}

pub(crate) struct NoopDestruction;

impl DestructionPort for NoopDestruction {
    fn destroy_resident_task(
        &self,
        _task_id: &str,
        _cause: DestroyCause,
    ) -> Result<(), crate::host::HostError> {
        Ok(())
    }
}
