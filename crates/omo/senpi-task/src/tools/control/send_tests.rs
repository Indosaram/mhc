//! `tools/control/send.test.ts`

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pretty_assertions::assert_eq;
use team_core::types::{RuntimeState, Task};

use crate::agents::interaction_policy_for_agent;
use crate::state::TaskStatus;
use crate::team::messaging::types::{SendTeamMessageInput, SendTeamMessageResult};
use crate::team::runtime_types::{CreateTeamResult, DeleteTeamResult};
use crate::tools::control::send::{
    MemberScopedTaskSendDeps, MemberScopedTaskSendTool, TASK_SEND_TOOL_NAME, TaskSendDeps, TaskSendTool,
    create_member_scoped_task_send_tool, create_task_send_tool, run_task_send,
};
use crate::tools::control::send_schema::{TaskSendInput, TaskSendMessage};
use crate::tools::control::tool_result::ToolResultContent;
use crate::tools::control::types::{
    ControlListScope, ControlSendInput, ControlTaskRecord, SendManager, SendOutcome, SendResultDetails,
    SendToolResult,
};
use crate::tools::team::types::{
    ActiveTeamSummary, CreateTeamTaskServiceInput, CreateTeamToolInput, DeleteTeamToolInput, DiscoveredTeamSpec,
    TeamServiceResult, TeamStatus, TeamTaskListFilter, TeamToolServiceError, TeamToolsService,
    UpdateTeamTaskServiceInput,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// spyManager
// ---------------------------------------------------------------------------

struct SpyManager {
    outcome: SendOutcome,
    send_calls: Mutex<Vec<ControlSendInput>>,
}

impl SpyManager {
    fn new(outcome: SendOutcome) -> Arc<Self> {
        Arc::new(Self {
            outcome,
            send_calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<ControlSendInput> {
        lock(&self.send_calls).clone()
    }
}

impl SendManager for SpyManager {
    fn send_to_task(&self, input: &ControlSendInput) -> Result<SendOutcome, String> {
        lock(&self.send_calls).push(input.clone());
        Ok(self.outcome.clone())
    }

    fn list(&self, _scope: &ControlListScope) -> Vec<ControlTaskRecord> {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Manager-like fake: models the steering semantics the TS tests exercise
// against the real manager (steer running, revive resident completed,
// refuse cancelled, scope checks, one-shot agent refusal, scoped list).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeStatus {
    Running,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone)]
struct FakeTask {
    task_id: String,
    name: Option<String>,
    parent_session_id: String,
    agent: Option<String>,
    status: FakeStatus,
    run_epoch: u64,
    steer_calls: Vec<String>,
    follow_up_calls: Vec<String>,
}

#[derive(Default)]
struct FakeManager {
    tasks: Mutex<Vec<FakeTask>>,
}

impl FakeManager {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn start(&self, parent: &str, name: Option<&str>, agent: Option<&str>) -> String {
        let mut tasks = lock(&self.tasks);
        let task_id = format!("st_{:08}", tasks.len() + 1);
        tasks.push(FakeTask {
            task_id: task_id.clone(),
            name: name.map(str::to_string),
            parent_session_id: parent.to_string(),
            agent: agent.map(str::to_string),
            status: FakeStatus::Running,
            run_epoch: 1,
            steer_calls: Vec::new(),
            follow_up_calls: Vec::new(),
        });
        task_id
    }

    fn set_status(&self, task_id: &str, status: FakeStatus) {
        if let Some(task) = lock(&self.tasks).iter_mut().find(|task| task.task_id == task_id) {
            task.status = status;
        }
    }

    fn task(&self, task_id: &str) -> FakeTask {
        lock(&self.tasks)
            .iter()
            .find(|task| task.task_id == task_id)
            .cloned()
            .expect("task present")
    }
}

impl SendManager for FakeManager {
    fn send_to_task(&self, input: &ControlSendInput) -> Result<SendOutcome, String> {
        let mut tasks = lock(&self.tasks);
        let Some(task) = tasks
            .iter_mut()
            .find(|task| task.task_id == input.id_or_name || task.name.as_deref() == Some(input.id_or_name.as_str()))
        else {
            return Ok(SendOutcome::NotFound {
                reason: format!("No task found for \"{}\".", input.id_or_name),
            });
        };
        if input.all_scope != Some(true)
            && input.caller_session_id.as_deref() != Some(task.parent_session_id.as_str())
        {
            return Ok(SendOutcome::ScopeDenied {
                task_id: task.task_id.clone(),
                owning_session_id: task.parent_session_id.clone(),
                reason: format!(
                    "Task {} is owned by session {}.",
                    task.task_id, task.parent_session_id
                ),
            });
        }
        if let Some(agent) = task.agent.clone()
            && let Some(policy) = interaction_policy_for_agent(&agent)
        {
            return Ok(SendOutcome::OneShotAgent {
                task_id: task.task_id.clone(),
                agent,
                message: policy.send_denial_reminder.to_string(),
            });
        }
        match task.status {
            FakeStatus::Running => {
                task.steer_calls.push(input.message.clone());
                Ok(SendOutcome::Steered {
                    task_id: task.task_id.clone(),
                    status: TaskStatus::Running,
                    delivered: "steer".to_string(),
                })
            }
            FakeStatus::Completed => {
                task.follow_up_calls.push(input.message.clone());
                task.status = FakeStatus::Running;
                task.run_epoch += 1;
                Ok(SendOutcome::Revived {
                    task_id: task.task_id.clone(),
                    run_epoch: task.run_epoch,
                })
            }
            FakeStatus::Cancelled => Ok(SendOutcome::NotContinuable {
                task_id: task.task_id.clone(),
                reason: "task was cancelled".to_string(),
                suggestion: "spawn a new task".to_string(),
            }),
        }
    }

    fn list(&self, scope: &ControlListScope) -> Vec<ControlTaskRecord> {
        lock(&self.tasks)
            .iter()
            .filter(|task| match scope {
                ControlListScope::All => true,
                ControlListScope::ParentSession { session_id } => &task.parent_session_id == session_id,
            })
            .map(|task| ControlTaskRecord {
                task_id: task.task_id.clone(),
                name: task.name.clone(),
                parent_session_id: task.parent_session_id.clone(),
                root_session_id: task.parent_session_id.clone(),
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// fakeTeamToolsService
// ---------------------------------------------------------------------------

fn fail<T>(name: &str) -> TeamServiceResult<T> {
    Err(TeamToolServiceError::new(format!(
        "fake TeamToolsService.{name} not configured"
    )))
}

struct FakeTeamToolsService;

impl TeamToolsService for FakeTeamToolsService {
    fn create_team(&self, _input: &CreateTeamToolInput) -> TeamServiceResult<CreateTeamResult> {
        fail("createTeam")
    }
    fn delete_team(&self, _input: &DeleteTeamToolInput) -> TeamServiceResult<DeleteTeamResult> {
        fail("deleteTeam")
    }
    fn send_message(
        &self,
        _team_run_id: &str,
        _input: &SendTeamMessageInput,
    ) -> TeamServiceResult<SendTeamMessageResult> {
        fail("sendMessage")
    }
    fn status(&self, _team_run_id: &str) -> TeamServiceResult<RuntimeState> {
        fail("status")
    }
    fn list_teams(&self) -> TeamServiceResult<Vec<ActiveTeamSummary>> {
        fail("listTeams")
    }
    fn create_task(&self, _team_run_id: &str, _input: &CreateTeamTaskServiceInput) -> TeamServiceResult<Task> {
        fail("createTask")
    }
    fn list_tasks(&self, _team_run_id: &str, _filter: Option<&TeamTaskListFilter>) -> TeamServiceResult<Vec<Task>> {
        fail("listTasks")
    }
    fn update_task(&self, _input: &UpdateTeamTaskServiceInput) -> TeamServiceResult<Task> {
        fail("updateTask")
    }
    fn get_task(&self, _team_run_id: &str, _task_id: &str) -> TeamServiceResult<Task> {
        fail("getTask")
    }
    fn request_shutdown(&self, _team_run_id: &str, _member: &str) -> TeamServiceResult<RuntimeState> {
        fail("requestShutdown")
    }
    fn approve_shutdown(&self, _team_run_id: &str, _member: &str) -> TeamServiceResult<RuntimeState> {
        fail("approveShutdown")
    }
    fn reject_shutdown(&self, _team_run_id: &str, _member: &str, _reason: &str) -> TeamServiceResult<RuntimeState> {
        fail("rejectShutdown")
    }
    fn aggregate_status(&self, _team_run_id: &str) -> TeamServiceResult<TeamStatus> {
        fail("aggregateStatus")
    }
    fn discover_team_specs(&self, _project_root: &Path) -> TeamServiceResult<Vec<DiscoveredTeamSpec>> {
        fail("discoverTeamSpecs")
    }
    fn load_team_spec_member_count(&self, _name: &str, _project_root: &Path) -> TeamServiceResult<usize> {
        fail("loadTeamSpec")
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn input(to: &str, message: Option<&str>, all_scope: Option<bool>) -> TaskSendInput {
    TaskSendInput {
        to: to.to_string(),
        message: message.map(|message| TaskSendMessage::Plain(message.to_string())),
        team_run_id: None,
        summary: None,
        all_scope,
    }
}

fn send(manager: &dyn SendManager, params: &TaskSendInput, caller: &str) -> SendToolResult {
    run_task_send(manager, params, Some(caller), None).expect("task_send succeeds")
}

fn text_of(result: &SendToolResult) -> String {
    match result.content.first() {
        Some(ToolResultContent::Text { text }) => text.clone(),
        None => String::new(),
    }
}

fn steered_outcome() -> SendOutcome {
    SendOutcome::Steered {
        task_id: "st_00000001".to_string(),
        status: TaskStatus::Running,
        delivered: "steer".to_string(),
    }
}

fn momus_reminder() -> String {
    interaction_policy_for_agent("momus")
        .expect("momus policy")
        .send_denial_reminder
        .to_string()
}

// ---------------------------------------------------------------------------
// runTaskSend
// ---------------------------------------------------------------------------

#[test]
fn given_task_send_tool_factories_when_tools_are_created_then_both_expose_custom_call_and_result_renderers() {
    let manager = SpyManager::new(SendOutcome::NotFound {
        reason: "unused".to_string(),
    });

    let lead_tool = create_task_send_tool(TaskSendDeps {
        manager: Arc::clone(&manager) as Arc<dyn SendManager>,
        team_routing: None,
        resolve_caller_session_id: None,
    });
    let member_tool = create_member_scoped_task_send_tool(MemberScopedTaskSendDeps {
        manager: manager as Arc<dyn SendManager>,
        service: Arc::new(FakeTeamToolsService),
        team_run_id: "team-run-1".to_string(),
        from: "atlas".to_string(),
        resolve_caller_session_id: None,
    });

    // Renderers are methods on both tool types; binding them proves they exist.
    let _lead_render_call = TaskSendTool::render_call;
    let _lead_render_result = TaskSendTool::render_result;
    let _member_render_call = MemberScopedTaskSendTool::render_call;
    let _member_render_result = MemberScopedTaskSendTool::render_result;
    assert_eq!(lead_tool.name, TASK_SEND_TOOL_NAME);
    assert_eq!(member_tool.name, TASK_SEND_TOOL_NAME);
    assert_eq!(member_tool.team_routing.team_run_id.as_deref(), Some("team-run-1"));
    assert_eq!(member_tool.team_routing.from, "atlas");
}

#[test]
fn given_a_running_child_when_a_message_is_sent_then_it_is_delivered_as_steer() {
    let manager = FakeManager::new();
    let task_id = manager.start("p1", None, None);

    let result = send(manager.as_ref(), &input(&task_id, Some("keep going"), None), "p1");

    assert_eq!(result.details.kind(), "steered");
    let SendResultDetails::Steered {
        task_id: steered_id,
        delivered,
        ..
    } = &result.details
    else {
        panic!("expected steered");
    };
    assert_eq!(delivered, "steer");
    assert_eq!(steered_id, &task_id);
    assert_eq!(manager.task(&task_id).steer_calls, vec!["keep going".to_string()]);
}

#[test]
fn given_a_caller_session_id_when_sending_then_caller_session_id_is_injected_into_the_steering_call() {
    let manager = SpyManager::new(steered_outcome());

    send(manager.as_ref(), &input("st_00000001", Some("hi"), None), "session-42");

    let calls = manager.calls();
    assert_eq!(calls[0].caller_session_id.as_deref(), Some("session-42"));
    assert_eq!(calls[0].all_scope, None);
}

#[test]
fn given_all_scope_true_when_sending_then_all_scope_is_forwarded_to_the_engine() {
    let manager = SpyManager::new(steered_outcome());

    send(manager.as_ref(), &input("st_00000001", Some("hi"), Some(true)), "session-42");

    let calls = manager.calls();
    assert_eq!(calls[0].all_scope, Some(true));
    assert_eq!(calls[0].message, "hi");
}

#[test]
fn given_plain_text_send_without_a_message_when_sent_then_it_is_rejected_before_routing() {
    let manager = SpyManager::new(SendOutcome::NotFound {
        reason: "unused".to_string(),
    });

    let result = send(manager.as_ref(), &input("alpha", None, None), "lead-session");

    assert_eq!(
        result.details,
        SendResultDetails::InvalidArguments {
            reason: "message is required".to_string(),
        }
    );
    assert_eq!(manager.calls(), Vec::<ControlSendInput>::new());
}

#[test]
fn given_a_child_owned_by_another_session_when_sent_without_all_scope_then_scope_is_denied_naming_the_owner() {
    let manager = FakeManager::new();
    let task_id = manager.start("owner-session", None, None);

    let result = send(manager.as_ref(), &input(&task_id, Some("hi"), None), "intruder-session");

    assert_eq!(result.details.kind(), "scope_denied");
    let SendResultDetails::ScopeDenied { owning_session_id, .. } = &result.details else {
        panic!("expected scope_denied");
    };
    assert_eq!(owning_session_id, "owner-session");
    assert!(text_of(&result).contains("owner-session"));
}

#[test]
fn given_an_unknown_name_when_sending_then_not_found_lists_this_sessions_task_names() {
    let manager = FakeManager::new();
    manager.start("p1", Some("alpha"), None);

    let result = send(manager.as_ref(), &input("ghost", Some("hi"), None), "p1");

    assert_eq!(result.details.kind(), "not_found");
    let SendResultDetails::NotFound { known_tasks, .. } = &result.details else {
        panic!("expected not_found");
    };
    assert!(known_tasks.iter().any(|known| known.contains("alpha")));
    assert!(text_of(&result).contains("alpha"));
}

#[test]
fn given_a_string_recipient_that_is_not_a_child_and_no_team_routing_when_sent_then_the_child_not_found_result_is_preserved()
 {
    let manager = SpyManager::new(SendOutcome::NotFound {
        reason: "No task found for \"ghost\".".to_string(),
    });

    let result = send(manager.as_ref(), &input("ghost", Some("hi"), None), "p1");

    assert_eq!(result.details.kind(), "not_found");
}

#[test]
fn given_a_cancelled_child_when_task_send_targets_it_then_it_is_not_continuable() {
    let manager = FakeManager::new();
    let task_id = manager.start("p1", None, None);
    manager.set_status(&task_id, FakeStatus::Cancelled);

    let result = send(manager.as_ref(), &input(&task_id, Some("revive?"), None), "p1");

    assert_eq!(result.details.kind(), "not_continuable");
}

// ---------------------------------------------------------------------------
// runTaskSend one-shot agent refusal
// ---------------------------------------------------------------------------

#[test]
fn given_a_running_momus_child_when_task_send_targets_it_then_the_send_is_refused_with_the_registry_reminder_and_nothing_is_delivered()
 {
    // given
    let manager = FakeManager::new();
    let task_id = manager.start("p1", None, Some("momus"));

    // when
    let result = send(manager.as_ref(), &input(&task_id, Some("please reconsider"), None), "p1");

    // then
    assert_eq!(result.details.kind(), "one_shot_agent");
    let SendResultDetails::OneShotAgent {
        task_id: refused_id,
        agent,
        ..
    } = &result.details
    else {
        panic!("expected one_shot_agent");
    };
    assert_eq!(refused_id, &task_id);
    assert_eq!(agent, "momus");
    let text = text_of(&result);
    assert_eq!(text, momus_reminder());
    assert!(text.contains("<system-reminder>"));
    let task = manager.task(&task_id);
    assert_eq!(task.steer_calls, Vec::<String>::new());
    assert_eq!(task.follow_up_calls, Vec::<String>::new());
}

#[test]
fn given_a_completed_resident_momus_child_when_task_send_targets_it_then_the_send_is_refused_with_the_registry_reminder_and_no_revive_occurs()
 {
    // given
    let manager = FakeManager::new();
    let task_id = manager.start("p1", None, Some("momus"));
    manager.set_status(&task_id, FakeStatus::Completed);
    assert_eq!(manager.task(&task_id).status, FakeStatus::Completed);

    // when
    let result = send(manager.as_ref(), &input(&task_id, Some("another round?"), None), "p1");

    // then
    assert_eq!(result.details.kind(), "one_shot_agent");
    assert_eq!(text_of(&result), momus_reminder());
    let task = manager.task(&task_id);
    assert_eq!(task.follow_up_calls, Vec::<String>::new());
    assert_eq!(task.status, FakeStatus::Completed);
}

#[test]
fn given_a_completed_resident_non_momus_child_when_task_send_targets_it_then_it_still_revives_regression_guard() {
    // given
    let manager = FakeManager::new();
    let task_id = manager.start("p1", None, Some("explore"));
    manager.set_status(&task_id, FakeStatus::Completed);
    assert_eq!(manager.task(&task_id).status, FakeStatus::Completed);

    // when
    let result = send(manager.as_ref(), &input(&task_id, Some("second pass"), None), "p1");

    // then
    assert_eq!(result.details.kind(), "revived");
    assert_eq!(manager.task(&task_id).follow_up_calls, vec!["second pass".to_string()]);
}
