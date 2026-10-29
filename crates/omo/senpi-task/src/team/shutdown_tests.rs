//! `team/shutdown.test.ts`

use std::cell::RefCell;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;
use team_core::team_state_store::{create_runtime_state, load_runtime_state, save_runtime_state};
use team_core::types::{RuntimeState, SpecSource, TeamSpec};

use crate::team::runtime_config::{TeamCoreConfig, TeamTaskBounds, to_team_core_config};
use crate::team::shutdown::{
    ApproveShutdownDeps, RejectShutdownDeps, RequestShutdownDeps, SenpiShutdownErrorCode, ShutdownFailure,
    ShutdownMessageKind, ShutdownOutboundMessage, approve_shutdown, reject_shutdown, request_shutdown,
};

fn temp_config() -> (TempDir, TeamCoreConfig) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bounds = TeamTaskBounds {
        max_members: 8,
        max_parallel_members: 4,
        max_wall_clock_minutes: 60,
    };
    let base_dir = dir.path().to_str().expect("utf8 path").to_string();
    let config = to_team_core_config(&bounds, &base_dir).expect("config");
    (dir, config)
}

fn state_json(state: &RuntimeState) -> Value {
    serde_json::to_value(state).expect("serialize state")
}

fn mutate_state(config: &TeamCoreConfig, team_run_id: &str, mutate: impl FnOnce(&mut Value)) {
    let current = load_runtime_state(team_run_id, config).expect("load");
    let mut value = state_json(&current);
    mutate(&mut value);
    let next = RuntimeState::safe_parse(&value).expect("valid state");
    save_runtime_state(&next, config).expect("save");
}

fn seed_active_team(config: &TeamCoreConfig, member_names: &[&str]) -> String {
    let members: Vec<Value> = member_names
        .iter()
        .map(|name| json!({ "kind": "category", "category": "coder", "prompt": "p", "name": name }))
        .collect();
    let spec = TeamSpec::safe_parse(&json!({
        "name": "shutdown-team",
        "leadAgentId": "lead",
        "members": members,
    }))
    .expect("spec");
    let state = create_runtime_state(&spec, Some("lead-session"), SpecSource::User, config).expect("create");
    let team_run_id = state_json(&state)
        .get("teamRunId")
        .and_then(Value::as_str)
        .expect("teamRunId")
        .to_string();
    mutate_state(config, &team_run_id, |value| {
        value["status"] = json!("active");
    });
    team_run_id
}

fn set_member_status(config: &TeamCoreConfig, team_run_id: &str, member_name: &str, status: &str) {
    mutate_state(config, team_run_id, |value| {
        if let Some(members) = value.get_mut("members").and_then(Value::as_array_mut) {
            for member in members.iter_mut() {
                if member.get("name").and_then(Value::as_str) == Some(member_name) {
                    member["status"] = json!(status);
                }
            }
        }
    });
}

fn requests(config: &TeamCoreConfig, team_run_id: &str) -> Vec<Value> {
    let state = load_runtime_state(team_run_id, config).expect("load");
    state_json(&state)
        .get("shutdownRequests")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn member_status(config: &TeamCoreConfig, team_run_id: &str, member_name: &str) -> Option<String> {
    let state = load_runtime_state(team_run_id, config).expect("load");
    state_json(&state)
        .get("members")
        .and_then(Value::as_array)
        .and_then(|members| {
            members
                .iter()
                .find(|member| member.get("name").and_then(Value::as_str) == Some(member_name))
                .and_then(|member| member.get("status").and_then(Value::as_str).map(str::to_string))
        })
}

fn is_absent(request: &Value, key: &str) -> bool {
    request.get(key).is_none_or(Value::is_null)
}

fn positive(request: &Value, key: &str) -> bool {
    request.get(key).and_then(Value::as_f64).is_some_and(|value| value > 0.0)
}

fn shutdown_code(failure: ShutdownFailure) -> SenpiShutdownErrorCode {
    match failure {
        ShutdownFailure::Shutdown(error) => {
            assert_eq!(error.name(), "SenpiShutdownError");
            error.code
        }
        other => panic!("expected SenpiShutdownError, got {other:?}"),
    }
}

#[test]
fn given_active_member_when_request_shutdown_then_pending_request_recorded_and_message_sent() {
    // given
    let (_dir, config) = temp_config();
    let team_run_id = seed_active_team(&config, &["alpha", "bravo"]);
    let sent: RefCell<Vec<ShutdownOutboundMessage>> = RefCell::new(Vec::new());
    let send = |message: &ShutdownOutboundMessage| -> Result<(), String> {
        sent.borrow_mut().push(message.clone());
        Ok(())
    };
    let deps = RequestShutdownDeps { config: &config, send_message: &send, now: None };

    // when
    request_shutdown(&team_run_id, "alpha", &deps).expect("request");

    // then
    let requests = requests(&config, &team_run_id);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].get("memberId").and_then(Value::as_str), Some("alpha"));
    assert!(is_absent(&requests[0], "approvedAt"));
    assert_eq!(
        sent.borrow().clone(),
        vec![ShutdownOutboundMessage {
            to: "alpha".to_string(),
            kind: ShutdownMessageKind::ShutdownRequest,
            body: String::new(),
        }]
    );
}

#[test]
fn given_pending_request_when_request_shutdown_again_then_idempotent() {
    // given
    let (_dir, config) = temp_config();
    let team_run_id = seed_active_team(&config, &["alpha"]);
    let sent: RefCell<Vec<ShutdownOutboundMessage>> = RefCell::new(Vec::new());
    let send = |message: &ShutdownOutboundMessage| -> Result<(), String> {
        sent.borrow_mut().push(message.clone());
        Ok(())
    };
    let deps = RequestShutdownDeps { config: &config, send_message: &send, now: None };
    request_shutdown(&team_run_id, "alpha", &deps).expect("request");

    // when
    request_shutdown(&team_run_id, "alpha", &deps).expect("request again");

    // then
    assert_eq!(requests(&config, &team_run_id).len(), 1);
    assert_eq!(sent.borrow().len(), 1);
}

#[test]
fn given_unknown_member_when_request_shutdown_then_unknown_member_error_and_nothing_sent() {
    // given
    let (_dir, config) = temp_config();
    let team_run_id = seed_active_team(&config, &["alpha"]);
    let sent: RefCell<Vec<ShutdownOutboundMessage>> = RefCell::new(Vec::new());
    let send = |message: &ShutdownOutboundMessage| -> Result<(), String> {
        sent.borrow_mut().push(message.clone());
        Ok(())
    };
    let deps = RequestShutdownDeps { config: &config, send_message: &send, now: None };

    // when
    let rejected = request_shutdown(&team_run_id, "ghost", &deps).expect_err("should fail");

    // then
    let code = shutdown_code(rejected);
    assert_eq!(code, SenpiShutdownErrorCode::UnknownMember);
    assert_eq!(code.as_str(), "unknown_member");
    assert_eq!(sent.borrow().len(), 0);
}

#[test]
fn given_pending_request_when_approve_shutdown_then_member_approved_task_cancelled_request_marked() {
    // given
    let (_dir, config) = temp_config();
    let team_run_id = seed_active_team(&config, &["alpha", "bravo"]);
    let sent: RefCell<Vec<ShutdownOutboundMessage>> = RefCell::new(Vec::new());
    let cancelled: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let send = |message: &ShutdownOutboundMessage| -> Result<(), String> {
        sent.borrow_mut().push(message.clone());
        Ok(())
    };
    let cancel = |member_name: &str| -> Result<(), String> {
        cancelled.borrow_mut().push(member_name.to_string());
        Ok(())
    };
    let request_deps = RequestShutdownDeps { config: &config, send_message: &send, now: None };
    request_shutdown(&team_run_id, "alpha", &request_deps).expect("request");

    // when
    let approve_deps = ApproveShutdownDeps {
        config: &config,
        send_message: &send,
        now: None,
        cancel_member_task: &cancel,
    };
    approve_shutdown(&team_run_id, "alpha", &approve_deps).expect("approve");

    // then
    assert_eq!(member_status(&config, &team_run_id, "alpha").as_deref(), Some("shutdown_approved"));
    assert_eq!(cancelled.borrow().clone(), vec!["alpha".to_string()]);
    let requests = requests(&config, &team_run_id);
    assert!(positive(&requests[0], "approvedAt"));
    assert!(
        sent.borrow()
            .iter()
            .any(|message| message.kind == ShutdownMessageKind::ShutdownApproved && message.to == "alpha")
    );
}

#[test]
fn given_no_pending_request_when_approve_shutdown_then_no_pending_request_error_and_no_cancel() {
    // given
    let (_dir, config) = temp_config();
    let team_run_id = seed_active_team(&config, &["alpha"]);
    let cancelled: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let send = |_message: &ShutdownOutboundMessage| -> Result<(), String> { Ok(()) };
    let cancel = |member_name: &str| -> Result<(), String> {
        cancelled.borrow_mut().push(member_name.to_string());
        Ok(())
    };
    let deps = ApproveShutdownDeps {
        config: &config,
        send_message: &send,
        now: None,
        cancel_member_task: &cancel,
    };

    // when
    let rejected = approve_shutdown(&team_run_id, "alpha", &deps).expect_err("should fail");

    // then
    let code = shutdown_code(rejected);
    assert_eq!(code, SenpiShutdownErrorCode::NoPendingRequest);
    assert_eq!(code.as_str(), "no_pending_request");
    assert_eq!(cancelled.borrow().len(), 0);
}

#[test]
fn given_completed_member_with_pending_request_when_approve_shutdown_then_completed_status_preserved() {
    // given
    let (_dir, config) = temp_config();
    let team_run_id = seed_active_team(&config, &["alpha"]);
    let send = |_message: &ShutdownOutboundMessage| -> Result<(), String> { Ok(()) };
    let cancel = |_member_name: &str| -> Result<(), String> { Ok(()) };
    let request_deps = RequestShutdownDeps { config: &config, send_message: &send, now: None };
    request_shutdown(&team_run_id, "alpha", &request_deps).expect("request");
    set_member_status(&config, &team_run_id, "alpha", "completed");

    // when
    let approve_deps = ApproveShutdownDeps {
        config: &config,
        send_message: &send,
        now: None,
        cancel_member_task: &cancel,
    };
    approve_shutdown(&team_run_id, "alpha", &approve_deps).expect("approve");

    // then
    assert_eq!(member_status(&config, &team_run_id, "alpha").as_deref(), Some("completed"));
    let requests = requests(&config, &team_run_id);
    assert!(positive(&requests[0], "approvedAt"));
}

#[test]
fn given_pending_request_when_reject_shutdown_then_member_left_running_and_reason_recorded() {
    // given
    let (_dir, config) = temp_config();
    let team_run_id = seed_active_team(&config, &["alpha"]);
    let sent: RefCell<Vec<ShutdownOutboundMessage>> = RefCell::new(Vec::new());
    let send = |message: &ShutdownOutboundMessage| -> Result<(), String> {
        sent.borrow_mut().push(message.clone());
        Ok(())
    };
    set_member_status(&config, &team_run_id, "alpha", "running");
    let deps: RejectShutdownDeps<'_> = RequestShutdownDeps { config: &config, send_message: &send, now: None };
    request_shutdown(&team_run_id, "alpha", &deps).expect("request");

    // when
    reject_shutdown(&team_run_id, "alpha", "still needed for the release", &deps).expect("reject");

    // then
    assert_eq!(member_status(&config, &team_run_id, "alpha").as_deref(), Some("running"));
    let requests = requests(&config, &team_run_id);
    assert!(positive(&requests[0], "rejectedAt"));
    assert_eq!(
        requests[0].get("rejectedReason").and_then(Value::as_str),
        Some("still needed for the release")
    );
    assert!(sent.borrow().iter().any(|message| {
        message.kind == ShutdownMessageKind::ShutdownRejected && message.body == "still needed for the release"
    }));
}

#[test]
fn given_no_pending_request_when_reject_shutdown_then_no_pending_request_error() {
    // given
    let (_dir, config) = temp_config();
    let team_run_id = seed_active_team(&config, &["alpha"]);
    let send = |_message: &ShutdownOutboundMessage| -> Result<(), String> { Ok(()) };
    let deps: RejectShutdownDeps<'_> = RequestShutdownDeps { config: &config, send_message: &send, now: None };

    // when
    let rejected = reject_shutdown(&team_run_id, "alpha", "no", &deps).expect_err("should fail");

    // then
    assert_eq!(shutdown_code(rejected), SenpiShutdownErrorCode::NoPendingRequest);
}

#[test]
fn given_rejected_request_when_request_shutdown_again_then_fresh_pending_request_allowed() {
    // given
    let (_dir, config) = temp_config();
    let team_run_id = seed_active_team(&config, &["alpha"]);
    let send = |_message: &ShutdownOutboundMessage| -> Result<(), String> { Ok(()) };
    let deps = RequestShutdownDeps { config: &config, send_message: &send, now: None };
    request_shutdown(&team_run_id, "alpha", &deps).expect("request");
    reject_shutdown(&team_run_id, "alpha", "later", &deps).expect("reject");

    // when
    request_shutdown(&team_run_id, "alpha", &deps).expect("request again");

    // then
    let requests = requests(&config, &team_run_id);
    assert_eq!(requests.len(), 2);
    assert!(is_absent(&requests[1], "approvedAt"));
    assert!(is_absent(&requests[1], "rejectedAt"));
}
