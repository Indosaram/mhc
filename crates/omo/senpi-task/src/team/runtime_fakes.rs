//! Test fakes for the team runtime (port of `team/__fixtures__/runtime-fakes.ts`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{SecondsFormat, Utc};
use tempfile::TempDir;

use crate::lifecycle::port::DestroyCause;
use crate::manager::execution_mode::ExecutionMode;
use crate::state::{ResidencyState, ResolvedModelRecord, TaskStatus};
use crate::team::member_projection::ResidentSessionRef;
use crate::team::runtime_config::TeamTaskBounds;
use crate::team::runtime_types::{
    TeamCancelOutcome, TeamMemberCancelPort, TeamMemberDestructionPort, TeamMemberReadPort, TeamMemberStartSpec,
    TeamMemberTaskRecord, TeamRuntimeManagerPort, TeamStartResult, TeamStartedMember,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) fn task_status(value: &str) -> TaskStatus {
    TaskStatus::parse(value).expect("known task status")
}

static CLEANUP_ROOTS: Mutex<Vec<TempDir>> = Mutex::new(Vec::new());

/// TS `cleanupTeamRuntimeTmp`; Rust tests own their temp dirs through `TempDir` drops, so this
/// fixture helper has no reader and is kept for parity with the TS fixture surface.
#[allow(dead_code)]
pub(crate) fn cleanup_team_runtime_tmp() {
    lock(&CLEANUP_ROOTS).clear();
}

pub(crate) fn temp_project_dir() -> PathBuf {
    let directory = tempfile::Builder::new()
        .prefix("senpi-team-runtime-")
        .tempdir()
        .expect("create temp project dir");
    let path = directory.path().to_path_buf();
    lock(&CLEANUP_ROOTS).push(directory);
    path
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TeamBoundsOverrides {
    pub(crate) max_members: Option<u64>,
    pub(crate) max_parallel_members: Option<u64>,
    pub(crate) max_wall_clock_minutes: Option<u64>,
}

pub(crate) fn team_bounds(team: TeamBoundsOverrides) -> TeamTaskBounds {
    TeamTaskBounds {
        max_members: team.max_members.unwrap_or(8),
        max_parallel_members: team.max_parallel_members.unwrap_or(4),
        max_wall_clock_minutes: team.max_wall_clock_minutes.unwrap_or(120),
    }
}

#[derive(Debug, Clone)]
pub(crate) enum StartBehavior {
    Ok {
        status: Option<TaskStatus>,
        resolved_model: Option<ResolvedModelRecord>,
    },
    Throw {
        message: String,
    },
    /// TS `startBehavior` variant; kept for parity with the TS fixture surface (no Rust reader).
    #[allow(dead_code)]
    Reject {
        result: TeamStartResult,
    },
}

pub(crate) type BeforeCancelReturn = Arc<dyn Fn(&str) + Send + Sync>;
pub(crate) type GetHook = Box<dyn Fn(&TeamMemberTaskRecord, usize) -> TeamMemberTaskRecord + Send>;

#[derive(Default)]
pub(crate) struct FakeTeamManagerOptions {
    pub(crate) behaviors: Vec<StartBehavior>,
    pub(crate) default_behavior: Option<StartBehavior>,
    pub(crate) before_cancel_return: Option<BeforeCancelReturn>,
}

fn build_record(
    task_id: &str,
    spec: &TeamMemberStartSpec,
    status: TaskStatus,
    resolved_model: Option<ResolvedModelRecord>,
) -> TeamMemberTaskRecord {
    let timestamp = now_iso();
    TeamMemberTaskRecord {
        task_id: task_id.to_string(),
        status,
        residency_state: ResidencyState::parse("resident").expect("known residency state"),
        created_at: timestamp.clone(),
        updated_at: timestamp,
        parent_session_id: spec.parent_session_id.clone(),
        root_session_id: spec
            .root_session_id
            .clone()
            .unwrap_or_else(|| spec.parent_session_id.clone()),
        depth: spec.depth,
        execution_mode: spec
            .execution_mode
            .unwrap_or_else(|| ExecutionMode::parse("in-process").expect("known execution mode")),
        model: spec.model.clone().unwrap_or_else(|| "fake/model".to_string()),
        child_session_id: Some(format!("sess-{task_id}")),
        resolved_model,
        name: spec.name.clone(),
        category: spec.category.clone(),
        agent_type: spec.subagent_type.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DestroyCall {
    pub(crate) task_id: String,
    pub(crate) cause: &'static str,
}

#[derive(Default)]
pub(crate) struct FakeDestruction {
    calls: Mutex<Vec<DestroyCall>>,
}

impl FakeDestruction {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn calls(&self) -> Vec<DestroyCall> {
        lock(&self.calls).clone()
    }
}

impl TeamMemberDestructionPort for FakeDestruction {
    fn destroy_resident_task(&self, task_id: &str, cause: DestroyCause) -> Result<(), String> {
        lock(&self.calls).push(DestroyCall {
            task_id: task_id.to_string(),
            cause: cause.as_str(),
        });
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CancelCall {
    pub(crate) task_id: String,
    pub(crate) reason: Option<String>,
}

#[derive(Default)]
struct FakeState {
    started: Vec<TeamMemberStartSpec>,
    cancelled: Vec<CancelCall>,
    records: HashMap<String, TeamMemberTaskRecord>,
    get_hooks: HashMap<String, GetHook>,
    get_counts: HashMap<String, usize>,
    counter: u64,
}

// Structural stand-in for the TaskManager the team runtime spawns members through. Records every
// start/cancel call and hands out deterministic st_ ids + child session ids so tests can assert the
// member -> task mapping and the rollback cancellations without a live in-process runner. cancel_task
// mirrors the production steering contract: terminal records noop instead of reporting cancelled.
#[derive(Default)]
pub(crate) struct FakeTeamManager {
    state: Mutex<FakeState>,
    options: FakeTeamManagerOptions,
}

impl FakeTeamManager {
    pub(crate) fn new(options: FakeTeamManagerOptions) -> Self {
        Self {
            state: Mutex::new(FakeState::default()),
            options,
        }
    }

    pub(crate) fn started(&self) -> Vec<TeamMemberStartSpec> {
        lock(&self.state).started.clone()
    }

    pub(crate) fn cancelled(&self) -> Vec<CancelCall> {
        lock(&self.state).cancelled.clone()
    }

    pub(crate) fn set_get_hook(&self, task_id: &str, hook: GetHook) {
        let mut state = lock(&self.state);
        state.get_hooks.insert(task_id.to_string(), hook);
        state.get_counts.insert(task_id.to_string(), 0);
    }

    pub(crate) fn set_status(&self, task_id: &str, status: TaskStatus) {
        if let Some(record) = lock(&self.state).records.get_mut(task_id) {
            record.status = status;
        }
    }

    pub(crate) fn set_residency(&self, task_id: &str, residency_state: ResidencyState) {
        if let Some(record) = lock(&self.state).records.get_mut(task_id) {
            record.residency_state = residency_state;
        }
    }
}

impl TeamMemberReadPort for FakeTeamManager {
    fn get(&self, task_id: &str) -> Option<TeamMemberTaskRecord> {
        let mut state = lock(&self.state);
        let record = state.records.get(task_id).cloned()?;
        if !state.get_hooks.contains_key(task_id) {
            return Some(record);
        }
        let read_count = state.get_counts.get(task_id).copied().unwrap_or(0) + 1;
        state.get_counts.insert(task_id.to_string(), read_count);
        let updated = state.get_hooks.get(task_id).map(|hook| hook(&record, read_count))?;
        state.records.insert(task_id.to_string(), updated.clone());
        Some(updated)
    }
}

impl TeamMemberCancelPort for FakeTeamManager {
    fn cancel_task(&self, id_or_name: &str, reason: Option<&str>) -> TeamCancelOutcome {
        let previous_status = {
            let mut state = lock(&self.state);
            state.cancelled.push(CancelCall {
                task_id: id_or_name.to_string(),
                reason: reason.map(str::to_string),
            });
            let Some(record) = state.records.get_mut(id_or_name) else {
                return TeamCancelOutcome::NotFound {
                    reason: format!("no task {id_or_name}"),
                };
            };
            let current = record.status.as_str();
            if current != "pending" && current != "running" {
                return TeamCancelOutcome::Noop {
                    task_id: id_or_name.to_string(),
                    status: record.status,
                    reason: format!("Task {id_or_name} is {current}, not running."),
                };
            }
            let previous = record.status;
            record.status = task_status("cancelled");
            record.updated_at = now_iso();
            previous
        };
        if let Some(hook) = &self.options.before_cancel_return {
            hook(id_or_name);
        }
        TeamCancelOutcome::Cancelled {
            task_id: id_or_name.to_string(),
            previous_status,
        }
    }
}

impl TeamRuntimeManagerPort for FakeTeamManager {
    fn start(&self, spec: &TeamMemberStartSpec) -> Result<TeamStartResult, String> {
        let mut state = lock(&self.state);
        let index = state.started.len();
        state.started.push(spec.clone());
        let behavior = self
            .options
            .behaviors
            .get(index)
            .or(self.options.default_behavior.as_ref())
            .cloned()
            .unwrap_or(StartBehavior::Ok {
                status: None,
                resolved_model: None,
            });
        let (status, resolved_model) = match behavior {
            StartBehavior::Throw { message } => return Err(message),
            StartBehavior::Reject { result } => return Ok(result),
            StartBehavior::Ok { status, resolved_model } => (status, resolved_model),
        };
        state.counter += 1;
        let task_id = format!("st_{:06}", state.counter);
        let status = status.unwrap_or_else(|| task_status("running"));
        state.records.insert(
            task_id.clone(),
            build_record(&task_id, spec, status, resolved_model.clone()),
        );
        let started_status = if status.as_str() == "pending" {
            task_status("pending")
        } else {
            task_status("running")
        };
        Ok(TeamStartResult::Started(TeamStartedMember {
            name: spec.name.clone().unwrap_or_else(|| task_id.clone()),
            task_id,
            status: started_status,
            resolved_model,
        }))
    }

    fn get_resident_handle(&self, task_id: &str) -> Option<ResidentSessionRef> {
        lock(&self.state).records.get(task_id).map(|record| ResidentSessionRef {
            session_id: record.child_session_id.clone(),
        })
    }
}
