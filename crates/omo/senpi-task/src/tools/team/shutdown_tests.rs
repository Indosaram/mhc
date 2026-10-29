//! `tools/team/shutdown.test.ts`

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::team::shutdown::{SenpiShutdownError, SenpiShutdownErrorCode};
use crate::tools::team::shutdown::{
    TeamApproveShutdownDetails, TeamApproveShutdownInput, TeamRejectShutdownDetails, TeamRejectShutdownInput,
    TeamShutdownRequestDetails, TeamShutdownRequestInput, run_team_approve_shutdown, run_team_reject_shutdown,
    run_team_shutdown_request, shutdown_error_to_service_error,
};
use crate::tools::team::team_tool_fakes::{
    FakeTeamServiceOverrides, create_fake_team_service, fake_runtime_state, fake_runtime_state_with,
};

// The Rust service seam carries `{ name, message }` only, so the fake errors use the real
// protocol message prefixes the runner classifies on.
fn unknown_member_error(member: &str) -> SenpiShutdownError {
    SenpiShutdownError::new(
        format!("unknown team member '{member}'"),
        SenpiShutdownErrorCode::UnknownMember,
        "run-1",
        member,
    )
}

fn no_pending_error(member: &str) -> SenpiShutdownError {
    SenpiShutdownError::new(
        format!("no pending shutdown request for '{member}'"),
        SenpiShutdownErrorCode::NoPendingRequest,
        "run-1",
        member,
    )
}

#[test]
fn given_a_member_when_request_runs_then_it_reports_requested() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        request_shutdown: Some(Box::new(|_, _| {
            Ok(fake_runtime_state_with(json!({ "status": "shutdown_requested" })))
        })),
        ..Default::default()
    });
    let result = run_team_shutdown_request(
        &service,
        &TeamShutdownRequestInput {
            team_run_id: "run-1".to_string(),
            member: "alpha".to_string(),
        },
    )
    .expect("request shutdown");
    assert_eq!(
        result.details,
        TeamShutdownRequestDetails::Requested {
            team_run_id: "run-1".to_string(),
            member: "alpha".to_string(),
        }
    );
    let calls = service.calls();
    assert_eq!(calls[0].method, "requestShutdown");
    assert_eq!(calls[0].args, vec![json!("run-1"), json!("alpha")]);
    let text = format!("{:?}", result.content);
    assert!(text.contains("'alpha'"), "{text}");
    assert!(text.contains("run-1"), "{text}");
}

#[test]
fn given_an_unknown_member_when_request_runs_then_it_reports_unknown_member() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        request_shutdown: Some(Box::new(|_, _| {
            Err(shutdown_error_to_service_error(&unknown_member_error("ghost")))
        })),
        ..Default::default()
    });
    let result = run_team_shutdown_request(
        &service,
        &TeamShutdownRequestInput {
            team_run_id: "run-1".to_string(),
            member: "ghost".to_string(),
        },
    )
    .expect("request shutdown");
    assert_eq!(result.details.kind(), "unknown_member");
    match &result.details {
        TeamShutdownRequestDetails::UnknownMember { member, .. } => assert_eq!(member, "ghost"),
        other => panic!("unexpected details: {other:?}"),
    }
}

#[test]
fn given_a_pending_request_when_approve_runs_then_it_reports_approved() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        approve_shutdown: Some(Box::new(|_, _| Ok(fake_runtime_state()))),
        ..Default::default()
    });
    let result = run_team_approve_shutdown(
        &service,
        &TeamApproveShutdownInput {
            team_run_id: "run-1".to_string(),
            member: "alpha".to_string(),
        },
    )
    .expect("approve shutdown");
    assert_eq!(
        result.details,
        TeamApproveShutdownDetails::Approved {
            team_run_id: "run-1".to_string(),
            member: "alpha".to_string(),
        }
    );
    let text = format!("{:?}", result.content);
    assert!(text.contains("'alpha'"), "{text}");
    assert!(text.contains("run-1"), "{text}");
}

#[test]
fn given_no_pending_request_when_approve_runs_then_it_reports_no_pending_request() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        approve_shutdown: Some(Box::new(|_, _| {
            Err(shutdown_error_to_service_error(&no_pending_error("alpha")))
        })),
        ..Default::default()
    });
    let result = run_team_approve_shutdown(
        &service,
        &TeamApproveShutdownInput {
            team_run_id: "run-1".to_string(),
            member: "alpha".to_string(),
        },
    )
    .expect("approve shutdown");
    assert_eq!(result.details.kind(), "no_pending_request");
    match &result.details {
        TeamApproveShutdownDetails::NoPendingRequest { member, .. } => assert_eq!(member, "alpha"),
        other => panic!("unexpected details: {other:?}"),
    }
}

#[test]
fn given_a_pending_request_when_reject_runs_then_it_reports_rejected_with_the_reason() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        reject_shutdown: Some(Box::new(|_, _, _| Ok(fake_runtime_state()))),
        ..Default::default()
    });
    let result = run_team_reject_shutdown(
        &service,
        &TeamRejectShutdownInput {
            team_run_id: "run-1".to_string(),
            member: "alpha".to_string(),
            reason: "keep going".to_string(),
        },
    )
    .expect("reject shutdown");
    assert_eq!(
        result.details,
        TeamRejectShutdownDetails::Rejected {
            team_run_id: "run-1".to_string(),
            member: "alpha".to_string(),
            reason: "keep going".to_string(),
        }
    );
    let calls = service.calls();
    assert_eq!(calls[0].method, "rejectShutdown");
    assert_eq!(calls[0].args, vec![json!("run-1"), json!("alpha"), json!("keep going")]);
    let text = format!("{:?}", result.content);
    assert!(text.contains("'alpha'"), "{text}");
    assert!(text.contains("run-1"), "{text}");
    assert!(text.contains("keep going"), "{text}");
}

#[test]
fn given_no_pending_request_when_reject_runs_then_it_reports_no_pending_request() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        reject_shutdown: Some(Box::new(|_, _, _| {
            Err(shutdown_error_to_service_error(&no_pending_error("alpha")))
        })),
        ..Default::default()
    });
    let result = run_team_reject_shutdown(
        &service,
        &TeamRejectShutdownInput {
            team_run_id: "run-1".to_string(),
            member: "alpha".to_string(),
            reason: "no".to_string(),
        },
    )
    .expect("reject shutdown");
    assert_eq!(result.details.kind(), "no_pending_request");
}
