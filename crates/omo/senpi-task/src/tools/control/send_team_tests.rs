//! `tools/control/send-team.test.ts`

use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::manager::TaskManager;
use crate::manager::manager_tests::fakes::{
    FakeRunner, Harness, base_spec, default_manager, started, status_of, wait_until,
};
use crate::manager::types::ManagerStartSpec;
use crate::team::messaging::types::{SendTeamMessageInput, SendTeamMessageResult};
use crate::team::normalize::TEAM_LEAD_SENTINEL;
use crate::tools::control::send::{
    MemberScopedTaskSendDeps, create_member_scoped_task_send_tool, run_task_send,
};
use crate::tools::control::send_schema::{StructuredMessageInput, TaskSendInput, TaskSendMessage};
use crate::tools::control::send_shutdown::{
    DefaultTeamRunIdResolution, TaskSendError, TaskSendTeamRouting,
};
use crate::tools::control::types::{
    CallerSessionResolver, ControlListScope, ControlSendInput, ControlTaskRecord, SendManager,
    SendOutcome, SendResultDetails, SendToolResult, SessionIdCarrier,
};
use crate::tools::team::types::TeamToolsService;

// The shared team-tool fakes live in `crate::tools::team::team_tool_fakes`, a crate-visible
// `#[cfg(test)]` module; reuse it instead of compiling the same source twice.
use crate::tools::team::team_tool_fakes::{
    FakeTeamService, FakeTeamServiceOverrides, create_fake_team_service, fake_runtime_state,
};

struct SpyManager {
    outcome: SendOutcome,
    send_calls: Mutex<Vec<ControlSendInput>>,
}

impl SendManager for SpyManager {
    fn send_to_task(&self, input: &ControlSendInput) -> Result<SendOutcome, String> {
        self.send_calls
            .lock()
            .expect("spy manager lock")
            .push(input.clone());
        Ok(self.outcome.clone())
    }

    fn list(&self, _scope: &ControlListScope) -> Vec<ControlTaskRecord> {
        Vec::new()
    }
}

fn spy_manager(reason: &str) -> Arc<SpyManager> {
    Arc::new(SpyManager {
        outcome: SendOutcome::NotFound {
            reason: reason.to_string(),
        },
        send_calls: Mutex::new(Vec::new()),
    })
}

fn service_with(overrides: FakeTeamServiceOverrides) -> Arc<FakeTeamService> {
    Arc::new(create_fake_team_service(overrides))
}

fn as_service(service: &Arc<FakeTeamService>) -> Arc<dyn TeamToolsService> {
    Arc::clone(service) as Arc<dyn TeamToolsService>
}

fn to_members_stub(message_id: &str, recipients: &[&str]) -> FakeTeamServiceOverrides {
    let message_id = message_id.to_string();
    let recipients: Vec<String> = recipients.iter().map(|name| (*name).to_string()).collect();
    FakeTeamServiceOverrides {
        send_message: Some(Box::new(move |_: &str, _: &SendTeamMessageInput| {
            Ok(SendTeamMessageResult::ToMembers {
                message_id: message_id.clone(),
                recipients: recipients.clone(),
            })
        })),
        ..FakeTeamServiceOverrides::default()
    }
}

fn request_shutdown_stub() -> FakeTeamServiceOverrides {
    FakeTeamServiceOverrides {
        request_shutdown: Some(Box::new(|_: &str, _: &str| Ok(fake_runtime_state()))),
        ..FakeTeamServiceOverrides::default()
    }
}

fn plain(to: &str, message: &str, team_run_id: Option<&str>, summary: Option<&str>) -> TaskSendInput {
    TaskSendInput {
        to: to.to_string(),
        message: Some(TaskSendMessage::Plain(message.to_string())),
        team_run_id: team_run_id.map(str::to_string),
        summary: summary.map(str::to_string),
        all_scope: None,
    }
}

fn shutdown_request(to: &str, team_run_id: Option<&str>) -> TaskSendInput {
    TaskSendInput {
        to: to.to_string(),
        message: Some(TaskSendMessage::Structured(
            StructuredMessageInput::ShutdownRequest { reason: None },
        )),
        team_run_id: team_run_id.map(str::to_string),
        summary: None,
        all_scope: None,
    }
}

fn lead_routing(service: &Arc<FakeTeamService>) -> TaskSendTeamRouting {
    TaskSendTeamRouting {
        service: as_service(service),
        from: TEAM_LEAD_SENTINEL.to_string(),
        team_run_id: None,
        resolve_default_team_run_id: None,
    }
}

fn ok(result: Result<SendToolResult, TaskSendError>) -> SendToolResult {
    match result {
        Ok(result) => result,
        Err(error) => panic!("run_task_send failed: {error:?}"),
    }
}

fn invalid_reason(result: &SendToolResult) -> String {
    match &result.details {
        SendResultDetails::InvalidArguments { reason } => reason.clone(),
        other => panic!("expected invalid_arguments, got {}", other.kind()),
    }
}

/// A running in-process child started through the real manager (`makeManager()` + `manager.start`).
struct Running {
    manager: TaskManager,
    runner: Arc<FakeRunner>,
    task_id: String,
    _harness: Harness,
}

fn start_running(parent_session_id: &str, name: &str) -> Running {
    let harness = default_manager();
    let spec = ManagerStartSpec {
        parent_session_id: parent_session_id.to_string(),
        name: Some(name.to_string()),
        ..base_spec()
    };
    let task = started(harness.manager.start(&spec));
    let task_id = task.task_id.clone();
    harness.in_process.wait_handle(&task_id);
    wait_until("task running", || {
        status_of(&harness.store, &task_id).is_some_and(|status| status.as_str() == "running")
    });
    Running {
        manager: harness.manager.clone(),
        runner: Arc::clone(&harness.in_process),
        task_id,
        _harness: harness,
    }
}

#[test]
fn given_a_string_recipient_that_is_a_team_member_when_team_routing_is_present_then_it_sends_a_team_message() {
    let manager = spy_manager("No task found for \"beta\".");
    let service = service_with(to_members_stub("msg-1", &["beta"]));

    let result = ok(run_task_send(
        manager.as_ref(),
        &plain("beta", "please report", Some("run-1"), Some("report")),
        Some("lead-session"),
        Some(&lead_routing(&service)),
    ));

    assert_eq!(result.details.kind(), "team_message");
    let calls = service.calls();
    assert_eq!(calls[0].method, "sendMessage");
    assert_eq!(
        calls[0].args,
        vec![
            json!("run-1"),
            json!({ "from": TEAM_LEAD_SENTINEL, "to": "beta", "body": "please report", "summary": "report" }),
        ]
    );
}

#[test]
fn given_a_team_route_without_a_run_id_when_child_lookup_misses_then_it_fails_before_service_calls() {
    let manager = spy_manager("No task found for \"beta\".");
    let service = service_with(FakeTeamServiceOverrides::default());

    let result = ok(run_task_send(
        manager.as_ref(),
        &plain("beta", "please report", None, None),
        Some("lead-session"),
        Some(&lead_routing(&service)),
    ));

    assert_eq!(result.details.kind(), "invalid_arguments");
    assert!(invalid_reason(&result).contains("team_run_id is required"));
    assert!(service.calls().is_empty());
}

#[test]
fn given_no_team_run_id_and_a_resolver_with_one_owned_team_when_child_lookup_misses_then_the_send_uses_the_resolved_team() {
    let manager = spy_manager("No task found for \"beta\".");
    let service = service_with(to_members_stub("msg-1", &["beta"]));
    let routing = TaskSendTeamRouting {
        resolve_default_team_run_id: Some(Arc::new(|| DefaultTeamRunIdResolution::Resolved {
            team_run_id: "run-1".to_string(),
        })),
        ..lead_routing(&service)
    };

    let result = ok(run_task_send(
        manager.as_ref(),
        &plain("beta", "please report", None, None),
        Some("lead-session"),
        Some(&routing),
    ));

    assert_eq!(result.details.kind(), "team_message");
    let calls = service.calls();
    assert_eq!(calls[0].method, "sendMessage");
    assert_eq!(calls[0].args[0], json!("run-1"));
    assert_eq!(calls[0].args[1]["to"], json!("beta"));
}

#[test]
fn given_no_team_run_id_and_an_ambiguous_resolver_when_child_lookup_misses_then_it_reports_the_owned_runs() {
    let manager = spy_manager("No task found for \"beta\".");
    let service = service_with(FakeTeamServiceOverrides::default());
    let routing = TaskSendTeamRouting {
        resolve_default_team_run_id: Some(Arc::new(|| DefaultTeamRunIdResolution::Ambiguous {
            reason: "Multiple active teams: run-1 ('alpha-team'), run-2 ('beta-team'). Pass team_run_id."
                .to_string(),
        })),
        ..lead_routing(&service)
    };

    let result = ok(run_task_send(
        manager.as_ref(),
        &plain("beta", "please report", None, None),
        Some("lead-session"),
        Some(&routing),
    ));

    assert_eq!(result.details.kind(), "invalid_arguments");
    let reason = invalid_reason(&result);
    assert!(reason.contains("run-1"));
    assert!(reason.contains("run-2"));
    assert!(service.calls().is_empty());
}

#[test]
fn given_no_team_run_id_and_a_resolver_reporting_no_owned_team_when_child_lookup_misses_then_it_falls_back_to_not_found() {
    let manager = spy_manager("No task found for \"beta\".");
    let service = service_with(FakeTeamServiceOverrides::default());
    let routing = TaskSendTeamRouting {
        resolve_default_team_run_id: Some(Arc::new(|| DefaultTeamRunIdResolution::None)),
        ..lead_routing(&service)
    };

    let result = ok(run_task_send(
        manager.as_ref(),
        &plain("beta", "please report", None, None),
        Some("lead-session"),
        Some(&routing),
    ));

    assert_eq!(result.details.kind(), "not_found");
    assert!(service.calls().is_empty());
}

#[test]
fn given_a_shutdown_request_without_team_run_id_and_a_resolver_with_one_owned_team_then_request_shutdown_uses_the_resolved_team() {
    let manager = spy_manager("unused");
    let service = service_with(request_shutdown_stub());
    let routing = TaskSendTeamRouting {
        resolve_default_team_run_id: Some(Arc::new(|| DefaultTeamRunIdResolution::Resolved {
            team_run_id: "run-1".to_string(),
        })),
        ..lead_routing(&service)
    };

    let result = ok(run_task_send(
        manager.as_ref(),
        &shutdown_request("alpha", None),
        Some("lead-session"),
        Some(&routing),
    ));

    assert_eq!(result.details.kind(), "shutdown_requested");
    let calls = service.calls();
    assert_eq!(calls[0].method, "requestShutdown");
    assert_eq!(calls[0].args, vec![json!("run-1"), json!("alpha")]);
}

#[test]
fn given_the_member_scoped_factory_when_created_then_it_exposes_the_shared_task_send_surface() {
    let manager = spy_manager("No task found for \"lead\".");
    let service = service_with(FakeTeamServiceOverrides::default());
    let resolver: CallerSessionResolver =
        Arc::new(|_: &dyn SessionIdCarrier| Some("member-session".to_string()));
    let tool = create_member_scoped_task_send_tool(MemberScopedTaskSendDeps {
        manager: Arc::clone(&manager) as Arc<dyn SendManager>,
        service: as_service(&service),
        team_run_id: "bound-run".to_string(),
        from: "alpha".to_string(),
        resolve_caller_session_id: Some(resolver),
    });

    assert_eq!(tool.name, "task_send");
    let keys: Vec<String> = tool.parameters["properties"]
        .as_object()
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default();
    assert!(keys.iter().any(|key| key == "to"));
    assert!(!keys.iter().any(|key| key == "deliver_as"));
    assert!(!tool.description.contains("followUp"));
}

#[test]
fn given_member_routing_with_a_bound_run_id_when_params_include_another_run_id_then_the_bound_run_id_wins() {
    let manager = spy_manager("No task found for \"lead\".");
    let service = service_with(FakeTeamServiceOverrides::default());
    let routing = TaskSendTeamRouting {
        service: as_service(&service),
        from: "alpha".to_string(),
        team_run_id: Some("bound-run".to_string()),
        resolve_default_team_run_id: None,
    };

    // The fake send stub is not installed; only the recorded call is asserted.
    let _ = run_task_send(
        manager.as_ref(),
        &plain("lead", "done", Some("wrong-run"), None),
        Some("member-session"),
        Some(&routing),
    );

    let calls = service.calls();
    assert_eq!(calls[0].method, "sendMessage");
    assert_eq!(
        calls[0].args,
        vec![
            json!("bound-run"),
            json!({ "from": "alpha", "to": "lead", "body": "done" }),
        ]
    );
}

#[test]
fn given_a_member_scoped_peer_send_when_the_recipient_is_running_then_the_steering_engine_delivers_as_steer() {
    let adapter = start_running("member-session", "lead");
    let service = service_with(FakeTeamServiceOverrides::default());
    let routing = TaskSendTeamRouting {
        service: as_service(&service),
        from: "alpha".to_string(),
        team_run_id: Some("bound-run".to_string()),
        resolve_default_team_run_id: None,
    };

    let result = ok(run_task_send(
        &adapter.manager,
        &plain("lead", "peer update", None, None),
        Some("member-session"),
        Some(&routing),
    ));

    match &result.details {
        SendResultDetails::Steered { delivered, .. } => assert_eq!(delivered, "steer"),
        other => panic!("expected steered, got {}", other.kind()),
    }
    let handle = adapter.runner.handle(&adapter.task_id).expect("expected member handle");
    assert_eq!(handle.steer_calls(), vec!["peer update".to_string()]);
    assert_eq!(handle.follow_up_calls(), Vec::<String>::new());
    handle.complete("done");
}

#[test]
fn given_a_lead_send_with_team_run_id_when_the_recipient_is_running_then_the_steering_engine_delivers_as_steer() {
    let adapter = start_running("lead-session", "beta");
    let service = service_with(FakeTeamServiceOverrides::default());

    let result = ok(run_task_send(
        &adapter.manager,
        &plain("beta", "lead update", Some("run-1"), None),
        Some("lead-session"),
        Some(&lead_routing(&service)),
    ));

    match &result.details {
        SendResultDetails::Steered { delivered, .. } => assert_eq!(delivered, "steer"),
        other => panic!("expected steered, got {}", other.kind()),
    }
    let handle = adapter.runner.handle(&adapter.task_id).expect("expected member handle");
    assert_eq!(handle.steer_calls(), vec!["lead update".to_string()]);
    assert_eq!(handle.follow_up_calls(), Vec::<String>::new());
    handle.complete("done");
}

#[test]
fn given_a_team_shutdown_message_when_lead_routing_sends_it_then_no_delivery_option_is_needed() {
    let manager = spy_manager("unused");
    let service = service_with(request_shutdown_stub());

    let result = ok(run_task_send(
        manager.as_ref(),
        &shutdown_request("beta", Some("run-1")),
        Some("lead-session"),
        Some(&lead_routing(&service)),
    ));

    match &result.details {
        SendResultDetails::ShutdownRequested { team_run_id, member } => {
            assert_eq!(team_run_id, "run-1");
            assert_eq!(member, "beta");
        }
        other => panic!("expected shutdown_requested, got {}", other.kind()),
    }
    let calls = service.calls();
    assert_eq!(calls[0].method, "requestShutdown");
    assert_eq!(calls[0].args, vec![Value::from("run-1"), Value::from("beta")]);
}
