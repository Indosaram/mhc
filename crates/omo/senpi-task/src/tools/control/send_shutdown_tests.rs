//! `tools/control/send-shutdown.test.ts`

use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::team::normalize::TEAM_LEAD_SENTINEL;
use crate::tools::control::send::run_task_send;
use crate::tools::control::send_schema::TaskSendInput;
use crate::tools::control::send_shutdown::{TaskSendError, TaskSendTeamRouting};
use crate::tools::control::tool_result::ToolResultContent;
use crate::tools::control::types::{
    ControlListScope, ControlSendInput, ControlTaskRecord, SendManager, SendOutcome, SendToolResult,
};
use crate::tools::team::types::TeamToolServiceError;

// The shared team tool fakes live in `crate::tools::team::team_tool_fakes`, a
// crate-visible `#[cfg(test)]` module; reuse it instead of compiling its source twice.
use crate::tools::team::team_tool_fakes::{
    FakeTeamService, FakeTeamServiceOverrides, create_fake_team_service, fake_runtime_state,
};

const RAW_MISSING_STATE_MESSAGE: &str =
    "ENOENT: no such file or directory, open '/private/secret/team/run-1/state.json'";

fn missing_state_error() -> TeamToolServiceError {
    TeamToolServiceError {
        name: "Error".to_string(),
        ..TeamToolServiceError::new(RAW_MISSING_STATE_MESSAGE)
    }
}

struct SpyManager {
    outcome: SendOutcome,
}

impl SendManager for SpyManager {
    fn send_to_task(&self, _input: &ControlSendInput) -> Result<SendOutcome, String> {
        Ok(self.outcome.clone())
    }

    fn list(&self, _scope: &ControlListScope) -> Vec<ControlTaskRecord> {
        Vec::new()
    }
}

fn spy_manager() -> SpyManager {
    SpyManager {
        outcome: SendOutcome::NotFound {
            reason: "unused".to_string(),
        },
    }
}

fn input(value: Value) -> TaskSendInput {
    serde_json::from_value(value).expect("valid task_send input")
}

fn lead_routing(service: &Arc<FakeTeamService>) -> TaskSendTeamRouting {
    TaskSendTeamRouting {
        service: service.clone(),
        from: TEAM_LEAD_SENTINEL.to_string(),
        team_run_id: None,
        resolve_default_team_run_id: None,
    }
}

fn details_json(result: &SendToolResult) -> Value {
    serde_json::to_value(&result.details).expect("serialize details")
}

fn first_text(result: &SendToolResult) -> String {
    result
        .content
        .first()
        .map(|ToolResultContent::Text { text }| text.clone())
        .unwrap_or_default()
}

fn serialized_result(result: &SendToolResult) -> String {
    format!("{}{}", details_json(result), first_text(result))
}

fn expect_missing_state_failure(result: &SendToolResult, operation: &str) {
    assert_eq!(
        details_json(result),
        json!({
            "kind": "shutdown_failed",
            "operation": operation,
            "team_run_id": "run-1",
            "member": "alpha",
            "code": "team_state_missing",
            "reason": "Team state is unavailable.",
        })
    );
    let serialized = serialized_result(result);
    assert!(!serialized.contains("ENOENT"));
    assert!(!serialized.contains("/private/secret"));
    assert!(!serialized.contains("state.json"));
}

#[test]
fn given_a_lead_shutdown_request_when_routed_through_task_send_then_request_shutdown_is_called() {
    let manager = spy_manager();
    let service = Arc::new(create_fake_team_service(FakeTeamServiceOverrides {
        request_shutdown: Some(Box::new(|_, _| Ok(fake_runtime_state()))),
        ..Default::default()
    }));

    let result = run_task_send(
        &manager,
        &input(json!({ "to": "alpha", "team_run_id": "run-1", "message": { "type": "shutdown_request" } })),
        Some("lead-session"),
        Some(&lead_routing(&service)),
    )
    .expect("task_send succeeds");

    assert_eq!(
        details_json(&result),
        json!({ "kind": "shutdown_requested", "team_run_id": "run-1", "member": "alpha" })
    );
    let calls = service.calls();
    assert_eq!(calls[0].method, "requestShutdown");
    assert_eq!(calls[0].args, vec![json!("run-1"), json!("alpha")]);
    let text = first_text(&result);
    assert!(text.contains("alpha"));
    assert!(text.contains("run-1"));
}

#[test]
fn given_a_lead_shutdown_response_approve_when_routed_through_task_send_then_approve_shutdown_is_called() {
    let manager = spy_manager();
    let service = Arc::new(create_fake_team_service(FakeTeamServiceOverrides {
        approve_shutdown: Some(Box::new(|_, _| Ok(fake_runtime_state()))),
        ..Default::default()
    }));

    let result = run_task_send(
        &manager,
        &input(json!({
            "to": "alpha",
            "team_run_id": "run-1",
            "message": { "type": "shutdown_response", "request_id": "ignored", "approve": true },
        })),
        Some("lead-session"),
        Some(&lead_routing(&service)),
    )
    .expect("task_send succeeds");

    assert_eq!(
        details_json(&result),
        json!({ "kind": "shutdown_responded", "team_run_id": "run-1", "member": "alpha", "approved": true })
    );
    let calls = service.calls();
    assert_eq!(calls[0].method, "approveShutdown");
    assert_eq!(calls[0].args, vec![json!("run-1"), json!("alpha")]);
    let text = first_text(&result);
    assert!(text.contains("alpha"));
    assert!(text.contains("run-1"));
}

#[test]
fn given_a_shutdown_response_reject_without_a_reason_when_routed_through_task_send_then_it_fails_before_reject_shutdown()
{
    let manager = spy_manager();
    let service = Arc::new(create_fake_team_service(FakeTeamServiceOverrides {
        reject_shutdown: Some(Box::new(|_, _, _| Ok(fake_runtime_state()))),
        ..Default::default()
    }));
    let routing = lead_routing(&service);

    let missing = run_task_send(
        &manager,
        &input(json!({
            "to": "alpha",
            "team_run_id": "run-1",
            "message": { "type": "shutdown_response", "approve": false },
        })),
        Some("lead-session"),
        Some(&routing),
    )
    .expect("task_send succeeds");
    let empty = run_task_send(
        &manager,
        &input(json!({
            "to": "alpha",
            "team_run_id": "run-1",
            "message": { "type": "shutdown_response", "approve": false, "reason": "" },
        })),
        Some("lead-session"),
        Some(&routing),
    )
    .expect("task_send succeeds");

    let expected = json!({
        "kind": "invalid_arguments",
        "reason": "reason is required when rejecting a shutdown",
    });
    assert_eq!(details_json(&missing), expected);
    assert_eq!(details_json(&empty), expected);
    assert!(service.calls().is_empty());
}

#[test]
fn given_a_shutdown_response_reject_with_whitespace_reason_when_routed_through_task_send_then_it_fails_before_reject_shutdown()
 {
    let manager = spy_manager();
    let service = Arc::new(create_fake_team_service(FakeTeamServiceOverrides {
        reject_shutdown: Some(Box::new(|_, _, _| Ok(fake_runtime_state()))),
        ..Default::default()
    }));

    let result = run_task_send(
        &manager,
        &input(json!({
            "to": "alpha",
            "team_run_id": "run-1",
            "message": { "type": "shutdown_response", "approve": false, "reason": "   " },
        })),
        Some("lead-session"),
        Some(&lead_routing(&service)),
    )
    .expect("task_send succeeds");

    assert_eq!(
        details_json(&result),
        json!({ "kind": "invalid_arguments", "reason": "reason is required when rejecting a shutdown" })
    );
    assert!(service.calls().is_empty());
}

#[test]
fn given_a_lead_shutdown_response_reject_with_a_reason_when_routed_through_task_send_then_reject_shutdown_is_called() {
    let manager = spy_manager();
    let service = Arc::new(create_fake_team_service(FakeTeamServiceOverrides {
        reject_shutdown: Some(Box::new(|_, _, _| Ok(fake_runtime_state()))),
        ..Default::default()
    }));

    let result = run_task_send(
        &manager,
        &input(json!({
            "to": "alpha",
            "team_run_id": "run-1",
            "message": { "type": "shutdown_response", "approve": false, "reason": "still needed" },
        })),
        Some("lead-session"),
        Some(&lead_routing(&service)),
    )
    .expect("task_send succeeds");

    assert_eq!(
        details_json(&result),
        json!({ "kind": "shutdown_responded", "team_run_id": "run-1", "member": "alpha", "approved": false })
    );
    let calls = service.calls();
    assert_eq!(calls[0].method, "rejectShutdown");
    assert_eq!(calls[0].args, vec![json!("run-1"), json!("alpha"), json!("still needed")]);
    let text = first_text(&result);
    assert!(text.contains("alpha"));
    assert!(text.contains("run-1"));
}

#[test]
fn given_shutdown_request_hits_missing_team_state_when_routed_through_task_send_then_it_returns_a_sanitized_structured_failure()
 {
    let manager = spy_manager();
    let service = Arc::new(create_fake_team_service(FakeTeamServiceOverrides {
        request_shutdown: Some(Box::new(|_, _| Err(missing_state_error()))),
        ..Default::default()
    }));

    let result = run_task_send(
        &manager,
        &input(json!({ "to": "alpha", "team_run_id": "run-1", "message": { "type": "shutdown_request" } })),
        Some("lead-session"),
        Some(&lead_routing(&service)),
    )
    .expect("task_send succeeds");

    expect_missing_state_failure(&result, "request");
}

#[test]
fn given_shutdown_response_approve_hits_missing_team_state_when_routed_through_task_send_then_it_returns_a_sanitized_structured_failure()
 {
    let manager = spy_manager();
    let service = Arc::new(create_fake_team_service(FakeTeamServiceOverrides {
        approve_shutdown: Some(Box::new(|_, _| Err(missing_state_error()))),
        ..Default::default()
    }));

    let result = run_task_send(
        &manager,
        &input(json!({
            "to": "alpha",
            "team_run_id": "run-1",
            "message": { "type": "shutdown_response", "approve": true },
        })),
        Some("lead-session"),
        Some(&lead_routing(&service)),
    )
    .expect("task_send succeeds");

    expect_missing_state_failure(&result, "approve");
}

#[test]
fn given_shutdown_response_reject_hits_missing_team_state_when_routed_through_task_send_then_it_returns_a_sanitized_structured_failure()
 {
    let manager = spy_manager();
    let service = Arc::new(create_fake_team_service(FakeTeamServiceOverrides {
        reject_shutdown: Some(Box::new(|_, _, _| Err(missing_state_error()))),
        ..Default::default()
    }));

    let result = run_task_send(
        &manager,
        &input(json!({
            "to": "alpha",
            "team_run_id": "run-1",
            "message": { "type": "shutdown_response", "approve": false, "reason": "still needed" },
        })),
        Some("lead-session"),
        Some(&lead_routing(&service)),
    )
    .expect("task_send succeeds");

    expect_missing_state_failure(&result, "reject");
}

#[test]
fn given_a_shutdown_domain_failure_when_routed_through_task_send_then_it_returns_stable_safe_failure_details() {
    let manager = spy_manager();
    let service = Arc::new(create_fake_team_service(FakeTeamServiceOverrides {
        request_shutdown: Some(Box::new(|_, _| {
            Err(TeamToolServiceError {
                name: "SenpiShutdownError".to_string(),
                ..TeamToolServiceError::new("unknown team member 'alpha' raw unknown member detail")
            })
        })),
        ..Default::default()
    }));

    let result = run_task_send(
        &manager,
        &input(json!({ "to": "alpha", "team_run_id": "run-1", "message": { "type": "shutdown_request" } })),
        Some("lead-session"),
        Some(&lead_routing(&service)),
    )
    .expect("task_send succeeds");

    assert_eq!(
        details_json(&result),
        json!({
            "kind": "shutdown_failed",
            "operation": "request",
            "team_run_id": "run-1",
            "member": "alpha",
            "code": "unknown_member",
            "reason": "Team member is unavailable.",
        })
    );
    assert!(!serialized_result(&result).contains("raw unknown member detail"));
}

#[test]
fn given_an_unexpected_shutdown_service_error_when_routed_through_task_send_then_the_original_exception_propagates() {
    let manager = spy_manager();
    let service = Arc::new(create_fake_team_service(FakeTeamServiceOverrides {
        request_shutdown: Some(Box::new(|_, _| {
            Err(TeamToolServiceError {
                name: "TypeError".to_string(),
                ..TeamToolServiceError::new("unexpected service failure")
            })
        })),
        ..Default::default()
    }));

    let outcome = run_task_send(
        &manager,
        &input(json!({ "to": "alpha", "team_run_id": "run-1", "message": { "type": "shutdown_request" } })),
        Some("lead-session"),
        Some(&lead_routing(&service)),
    );

    match outcome {
        Err(TaskSendError::Service(error)) => {
            assert_eq!(error.name, "TypeError");
            assert_eq!(error.message, "unexpected service failure");
        }
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("expected task_send to reject"),
    }
}

#[test]
fn given_structured_message_with_no_team_routing_when_sent_then_it_reports_not_in_a_team() {
    let manager = spy_manager();

    let result = run_task_send(
        &manager,
        &input(json!({ "to": "alpha", "message": { "type": "shutdown_request" } })),
        Some("lead-session"),
        None,
    )
    .expect("task_send succeeds");

    assert_eq!(
        details_json(&result),
        json!({ "kind": "invalid_arguments", "reason": "not in a team" })
    );
}

#[test]
fn given_member_scoped_task_send_when_it_sends_a_structured_shutdown_message_then_shutdown_is_lead_only() {
    let manager = spy_manager();
    let service = Arc::new(create_fake_team_service(FakeTeamServiceOverrides {
        request_shutdown: Some(Box::new(|_, _| Ok(fake_runtime_state()))),
        ..Default::default()
    }));
    let routing = TaskSendTeamRouting {
        service: service.clone(),
        from: "alpha".to_string(),
        team_run_id: Some("run-1".to_string()),
        resolve_default_team_run_id: None,
    };

    let result = run_task_send(
        &manager,
        &input(json!({ "to": "alpha", "message": { "type": "shutdown_request" } })),
        Some("member-session"),
        Some(&routing),
    )
    .expect("task_send succeeds");

    assert_eq!(
        details_json(&result),
        json!({ "kind": "invalid_arguments", "reason": "shutdown is lead-only" })
    );
    assert!(service.calls().is_empty());
}
