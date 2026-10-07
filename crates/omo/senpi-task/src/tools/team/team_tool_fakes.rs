//! Recording fakes for the team-tools service (TS `tools/team/__fixtures__/team-tool-fakes.ts`).

use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{Map, Value, json};
use team_core::types::{MemberStatus, RuntimeState, Task};

use crate::team::messaging::types::{SendTeamMessageInput, SendTeamMessageResult};
use crate::team::member_map::MemberTaskMap;
use crate::team::runtime_types::{CreateTeamResult, CreatedMemberInfo, CreatedMemberRole, DeleteTeamResult};
use crate::tools::team::types::{
    ActiveTeamScope, ActiveTeamSummary, CreateTeamTaskServiceInput, CreateTeamToolInput, DeleteTeamToolInput,
    DiscoveredTeamSpec, TeamServiceResult, TeamStatus, TeamTaskListFilter, TeamToolServiceError,
    TeamToolsService, UpdateTeamTaskServiceInput,
};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServiceCall {
    pub(crate) method: String,
    pub(crate) args: Vec<Value>,
}

pub(crate) type CreateTeamStub = Box<dyn Fn(&CreateTeamToolInput) -> TeamServiceResult<CreateTeamResult> + Send + Sync>;
pub(crate) type DeleteTeamStub = Box<dyn Fn(&DeleteTeamToolInput) -> TeamServiceResult<DeleteTeamResult> + Send + Sync>;
pub(crate) type SendMessageStub =
    Box<dyn Fn(&str, &SendTeamMessageInput) -> TeamServiceResult<SendTeamMessageResult> + Send + Sync>;
pub(crate) type StatusStub = Box<dyn Fn(&str) -> TeamServiceResult<RuntimeState> + Send + Sync>;
pub(crate) type ListTeamsStub = Box<dyn Fn() -> TeamServiceResult<Vec<ActiveTeamSummary>> + Send + Sync>;
pub(crate) type CreateTaskStub =
    Box<dyn Fn(&str, &CreateTeamTaskServiceInput) -> TeamServiceResult<Task> + Send + Sync>;
pub(crate) type ListTasksStub =
    Box<dyn Fn(&str, Option<&TeamTaskListFilter>) -> TeamServiceResult<Vec<Task>> + Send + Sync>;
pub(crate) type UpdateTaskStub = Box<dyn Fn(&UpdateTeamTaskServiceInput) -> TeamServiceResult<Task> + Send + Sync>;
pub(crate) type GetTaskStub = Box<dyn Fn(&str, &str) -> TeamServiceResult<Task> + Send + Sync>;
pub(crate) type ShutdownStub = Box<dyn Fn(&str, &str) -> TeamServiceResult<RuntimeState> + Send + Sync>;
pub(crate) type RejectShutdownStub = Box<dyn Fn(&str, &str, &str) -> TeamServiceResult<RuntimeState> + Send + Sync>;
pub(crate) type AggregateStatusStub = Box<dyn Fn(&str) -> TeamServiceResult<TeamStatus> + Send + Sync>;
pub(crate) type DiscoverSpecsStub =
    Box<dyn Fn(&Path) -> TeamServiceResult<Vec<DiscoveredTeamSpec>> + Send + Sync>;
pub(crate) type LoadSpecCountStub =
    Box<dyn Fn(&str, &Path) -> TeamServiceResult<usize> + Send + Sync>;

/// `Partial<TeamToolsService>`: only the stubbed methods succeed; the rest fail as "not stubbed".
#[derive(Default)]
pub(crate) struct FakeTeamServiceOverrides {
    pub(crate) create_team: Option<CreateTeamStub>,
    pub(crate) delete_team: Option<DeleteTeamStub>,
    pub(crate) send_message: Option<SendMessageStub>,
    pub(crate) status: Option<StatusStub>,
    pub(crate) list_teams: Option<ListTeamsStub>,
    pub(crate) create_task: Option<CreateTaskStub>,
    pub(crate) list_tasks: Option<ListTasksStub>,
    pub(crate) update_task: Option<UpdateTaskStub>,
    pub(crate) get_task: Option<GetTaskStub>,
    pub(crate) request_shutdown: Option<ShutdownStub>,
    pub(crate) approve_shutdown: Option<ShutdownStub>,
    pub(crate) reject_shutdown: Option<RejectShutdownStub>,
    pub(crate) aggregate_status: Option<AggregateStatusStub>,
    pub(crate) discover_team_specs: Option<DiscoverSpecsStub>,
    pub(crate) load_team_spec_member_count: Option<LoadSpecCountStub>,
}

// A recording fake for the team-tools service. Every method fails "not stubbed" by default so a tool
// under test must be pointed at exactly the calls it should make; override only the methods a case
// exercises. All calls are recorded so tests assert the closure-bound arguments (team run id, from).
pub(crate) struct FakeTeamService {
    calls: Mutex<Vec<ServiceCall>>,
    overrides: FakeTeamServiceOverrides,
}

pub(crate) fn create_fake_team_service(overrides: FakeTeamServiceOverrides) -> FakeTeamService {
    FakeTeamService {
        calls: Mutex::new(Vec::new()),
        overrides,
    }
}

fn to_arg<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("serialize fake service arg")
}

fn send_input_arg(input: &SendTeamMessageInput) -> Value {
    let mut record = Map::new();
    record.insert("from".to_string(), Value::String(input.from.clone()));
    record.insert("to".to_string(), Value::String(input.to.clone()));
    record.insert("body".to_string(), Value::String(input.body.clone()));
    if let Some(summary) = &input.summary {
        record.insert("summary".to_string(), Value::String(summary.clone()));
    }
    Value::Object(record)
}

fn not_stubbed(method: &str) -> TeamToolServiceError {
    TeamToolServiceError::new(format!("fake team service: {method} not stubbed"))
}

impl FakeTeamService {
    pub(crate) fn calls(&self) -> Vec<ServiceCall> {
        self.calls.lock().expect("fake team service calls lock").clone()
    }

    fn record(&self, method: &str, args: Vec<Value>) {
        self.calls
            .lock()
            .expect("fake team service calls lock")
            .push(ServiceCall {
                method: method.to_string(),
                args,
            });
    }
}

impl TeamToolsService for FakeTeamService {
    fn create_team(&self, input: &CreateTeamToolInput) -> TeamServiceResult<CreateTeamResult> {
        self.record("createTeam", vec![to_arg(input)]);
        match &self.overrides.create_team {
            Some(stub) => stub(input),
            None => Err(not_stubbed("createTeam")),
        }
    }

    fn delete_team(&self, input: &DeleteTeamToolInput) -> TeamServiceResult<DeleteTeamResult> {
        self.record("deleteTeam", vec![to_arg(input)]);
        match &self.overrides.delete_team {
            Some(stub) => stub(input),
            None => Err(not_stubbed("deleteTeam")),
        }
    }

    fn send_message(
        &self,
        team_run_id: &str,
        input: &SendTeamMessageInput,
    ) -> TeamServiceResult<SendTeamMessageResult> {
        self.record("sendMessage", vec![json!(team_run_id), send_input_arg(input)]);
        match &self.overrides.send_message {
            Some(stub) => stub(team_run_id, input),
            None => Err(not_stubbed("sendMessage")),
        }
    }

    fn status(&self, team_run_id: &str) -> TeamServiceResult<RuntimeState> {
        self.record("status", vec![json!(team_run_id)]);
        match &self.overrides.status {
            Some(stub) => stub(team_run_id),
            None => Err(not_stubbed("status")),
        }
    }

    fn list_teams(&self) -> TeamServiceResult<Vec<ActiveTeamSummary>> {
        self.record("listTeams", Vec::new());
        match &self.overrides.list_teams {
            Some(stub) => stub(),
            None => Err(not_stubbed("listTeams")),
        }
    }

    fn create_task(&self, team_run_id: &str, input: &CreateTeamTaskServiceInput) -> TeamServiceResult<Task> {
        self.record("createTask", vec![json!(team_run_id), to_arg(input)]);
        match &self.overrides.create_task {
            Some(stub) => stub(team_run_id, input),
            None => Err(not_stubbed("createTask")),
        }
    }

    fn list_tasks(&self, team_run_id: &str, filter: Option<&TeamTaskListFilter>) -> TeamServiceResult<Vec<Task>> {
        let filter_arg = filter.map_or(Value::Null, to_arg);
        self.record("listTasks", vec![json!(team_run_id), filter_arg]);
        match &self.overrides.list_tasks {
            Some(stub) => stub(team_run_id, filter),
            None => Err(not_stubbed("listTasks")),
        }
    }

    fn update_task(&self, input: &UpdateTeamTaskServiceInput) -> TeamServiceResult<Task> {
        self.record("updateTask", vec![to_arg(input)]);
        match &self.overrides.update_task {
            Some(stub) => stub(input),
            None => Err(not_stubbed("updateTask")),
        }
    }

    fn get_task(&self, team_run_id: &str, task_id: &str) -> TeamServiceResult<Task> {
        self.record("getTask", vec![json!(team_run_id), json!(task_id)]);
        match &self.overrides.get_task {
            Some(stub) => stub(team_run_id, task_id),
            None => Err(not_stubbed("getTask")),
        }
    }

    fn request_shutdown(&self, team_run_id: &str, member: &str) -> TeamServiceResult<RuntimeState> {
        self.record("requestShutdown", vec![json!(team_run_id), json!(member)]);
        match &self.overrides.request_shutdown {
            Some(stub) => stub(team_run_id, member),
            None => Err(not_stubbed("requestShutdown")),
        }
    }

    fn approve_shutdown(&self, team_run_id: &str, member: &str) -> TeamServiceResult<RuntimeState> {
        self.record("approveShutdown", vec![json!(team_run_id), json!(member)]);
        match &self.overrides.approve_shutdown {
            Some(stub) => stub(team_run_id, member),
            None => Err(not_stubbed("approveShutdown")),
        }
    }

    fn reject_shutdown(&self, team_run_id: &str, member: &str, reason: &str) -> TeamServiceResult<RuntimeState> {
        self.record("rejectShutdown", vec![json!(team_run_id), json!(member), json!(reason)]);
        match &self.overrides.reject_shutdown {
            Some(stub) => stub(team_run_id, member, reason),
            None => Err(not_stubbed("rejectShutdown")),
        }
    }

    fn aggregate_status(&self, team_run_id: &str) -> TeamServiceResult<TeamStatus> {
        self.record("aggregateStatus", vec![json!(team_run_id)]);
        match &self.overrides.aggregate_status {
            Some(stub) => stub(team_run_id),
            None => Err(not_stubbed("aggregateStatus")),
        }
    }

    fn discover_team_specs(&self, project_root: &Path) -> TeamServiceResult<Vec<DiscoveredTeamSpec>> {
        self.record("discoverTeamSpecs", vec![json!(project_root.display().to_string())]);
        match &self.overrides.discover_team_specs {
            Some(stub) => stub(project_root),
            None => Err(not_stubbed("discoverTeamSpecs")),
        }
    }

    fn load_team_spec_member_count(&self, name: &str, project_root: &Path) -> TeamServiceResult<usize> {
        self.record("loadTeamSpec", vec![json!(name), json!(project_root.display().to_string())]);
        match &self.overrides.load_team_spec_member_count {
            Some(stub) => stub(name, project_root),
            None => Err(not_stubbed("loadTeamSpec")),
        }
    }
}

fn merge_overrides(mut base: Value, overrides: Value) -> Value {
    if let (Some(target), Value::Object(extra)) = (base.as_object_mut(), overrides) {
        for (key, value) in extra {
            target.insert(key, value);
        }
    }
    base
}

fn runtime_state_json() -> Value {
    json!({
        "version": 1,
        "teamRunId": "00000000-0000-4000-8000-000000000000",
        "teamName": "demo",
        "specSource": "user",
        "createdAt": 1,
        "status": "active",
        "members": [
            { "name": "alpha", "agentType": "general-purpose", "status": "running", "pendingInjectedMessageIds": [] },
            { "name": "beta", "agentType": "general-purpose", "status": "idle", "pendingInjectedMessageIds": [] },
        ],
        "shutdownRequests": [],
        "bounds": {
            "maxMembers": 8,
            "maxParallelMembers": 4,
            "maxMessagesPerRun": 10000,
            "maxWallClockMinutes": 120,
            "maxMemberTurns": 500,
        },
    })
}

/// `fakeRuntimeState(overrides)`: `overrides` is a JSON object of top-level RuntimeState fields.
pub(crate) fn fake_runtime_state_with(overrides: Value) -> RuntimeState {
    let value = merge_overrides(runtime_state_json(), overrides);
    RuntimeState::safe_parse(&value).unwrap_or_else(|issues| {
        panic!(
            "fake runtime state invalid: {:?}",
            issues.first().map(|issue| issue.message.clone())
        )
    })
}

pub(crate) fn fake_runtime_state() -> RuntimeState {
    fake_runtime_state_with(Value::Object(Map::new()))
}

fn task_json() -> Value {
    json!({
        "version": 1,
        "id": "task-1",
        "subject": "do the thing",
        "description": "details",
        "status": "pending",
        "blocks": [],
        "blockedBy": [],
        "createdAt": 1,
        "updatedAt": 1,
    })
}

/// `fakeTask(overrides)`: `overrides` is a JSON object of top-level Task fields.
pub(crate) fn fake_task_with(overrides: Value) -> Task {
    let value = merge_overrides(task_json(), overrides);
    Task::safe_parse(&value).unwrap_or_else(|issues| {
        panic!(
            "fake task invalid: {:?}",
            issues.first().map(|issue| issue.message.clone())
        )
    })
}

pub(crate) fn fake_task() -> Task {
    fake_task_with(Value::Object(Map::new()))
}

/// Default created member; override fields with struct update syntax.
pub(crate) fn fake_created_member() -> CreatedMemberInfo {
    CreatedMemberInfo {
        name: "alpha".to_string(),
        task_id: "st_a".to_string(),
        status: MemberStatus::Running,
        role: CreatedMemberRole::Category {
            category: "deep".to_string(),
        },
        model: None,
        prompt_excerpt: None,
        task_summary: None,
    }
}

/// Default create result; override fields with struct update syntax.
pub(crate) fn fake_create_result() -> CreateTeamResult {
    let mut member_task_ids = MemberTaskMap::new();
    member_task_ids.insert("alpha".to_string(), "st_a".to_string());
    member_task_ids.insert("beta".to_string(), "st_b".to_string());
    CreateTeamResult {
        runtime_state: fake_runtime_state(),
        member_task_ids,
        members: vec![
            fake_created_member(),
            CreatedMemberInfo {
                name: "beta".to_string(),
                task_id: "st_b".to_string(),
                status: MemberStatus::Idle,
                role: CreatedMemberRole::SubagentType {
                    subagent_type: "sisyphus".to_string(),
                },
                ..fake_created_member()
            },
        ],
    }
}

/// Default delete result; override fields with struct update syntax.
pub(crate) fn fake_delete_result() -> DeleteTeamResult {
    DeleteTeamResult {
        team_run_id: "00000000-0000-4000-8000-000000000000".to_string(),
        cancelled_task_ids: vec!["st_a".to_string()],
    }
}

/// Default active team summary; override fields with struct update syntax.
/// TS `fakeSummary`; the Rust team-tool tests build summaries inline, so this fixture helper has
/// no reader and is kept for parity with the TS fixture surface.
#[allow(dead_code)]
pub(crate) fn fake_summary() -> ActiveTeamSummary {
    ActiveTeamSummary {
        team_run_id: "run-1".to_string(),
        team_name: "demo".to_string(),
        status: "active".to_string(),
        member_count: 2,
        scope: ActiveTeamScope::User,
        lead_session_id: None,
    }
}

/// TS `fakeSendResult`; kept for parity with the TS fixture surface (no Rust reader).
#[allow(dead_code)]
pub(crate) fn fake_send_result(result: SendTeamMessageResult) -> SendTeamMessageResult {
    result
}
