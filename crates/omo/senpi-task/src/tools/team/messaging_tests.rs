//! `tools/team/messaging.test.ts`

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::team::messaging::types::SendTeamMessageResult;
use crate::team::normalize::TEAM_LEAD_SENTINEL;
use crate::tools::team::messaging::{TeamSendInput, run_team_send};
use crate::tools::team::team_tool_fakes::{FakeTeamServiceOverrides, create_fake_team_service};
use crate::tools::team::types::TeamToolServiceError;

fn named_error(name: &str, message: &str) -> TeamToolServiceError {
    let mut error = TeamToolServiceError::new(message.to_string());
    error.name = name.to_string();
    error
}

fn input(to: &str, body: &str) -> TeamSendInput {
    TeamSendInput {
        to: to.to_string(),
        body: body.to_string(),
        summary: None,
    }
}

#[test]
fn given_a_message_to_the_lead_when_it_is_enqueued_then_it_reports_the_lead_inbox_result() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        send_message: Some(Box::new(|_, _| {
            Ok(SendTeamMessageResult::ToLead {
                message_id: "m1".to_string(),
            })
        })),
        ..Default::default()
    });

    let result = run_team_send(&service, "run-1", TEAM_LEAD_SENTINEL, &input("lead", "hi")).expect("send");

    assert_eq!(
        serde_json::to_value(&result.content).expect("content"),
        json!([{ "type": "text", "text": "Message enqueued to lead (id: m1)." }])
    );
    assert_eq!(
        serde_json::to_value(&result.details).expect("details"),
        json!({ "kind": "to_lead", "message_id": "m1" })
    );
}

#[test]
fn given_a_member_direction_message_when_enqueued_then_it_reports_the_recipient_list() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        send_message: Some(Box::new(|_, _| {
            Ok(SendTeamMessageResult::ToMembers {
                message_id: "m2".to_string(),
                recipients: vec!["beta".to_string()],
            })
        })),
        ..Default::default()
    });

    let result = run_team_send(&service, "run-1", TEAM_LEAD_SENTINEL, &input("beta", "go")).expect("send");

    assert_eq!(
        serde_json::to_value(&result.content).expect("content"),
        json!([{ "type": "text", "text": "Message enqueued to 1 recipient(s): beta (id: m2)." }])
    );
    assert_eq!(
        serde_json::to_value(&result.details).expect("details"),
        json!({ "kind": "to_members", "message_id": "m2", "recipients": ["beta"] })
    );
}

#[test]
fn given_a_recipient_backpressure_error_when_send_runs_then_it_surfaces_recipient_backpressure() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        send_message: Some(Box::new(|_, _| {
            Err(named_error(
                "RecipientBackpressureError",
                "recipient inbox full (backpressure)",
            ))
        })),
        ..Default::default()
    });

    let result = run_team_send(&service, "run-1", TEAM_LEAD_SENTINEL, &input("beta", "x")).expect("send");

    assert_eq!(result.details.kind(), "recipient_backpressure");
}

#[test]
fn given_a_non_lead_broadcast_when_send_runs_then_it_surfaces_broadcast_denied() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        send_message: Some(Box::new(|_, _| {
            Err(named_error("BroadcastNotPermittedError", "broadcast requires lead role"))
        })),
        ..Default::default()
    });

    let result = run_team_send(&service, "run-1", "alpha", &input("*", "x")).expect("send");

    assert_eq!(result.details.kind(), "broadcast_denied");
}

#[test]
fn given_a_member_targeting_a_non_member_when_send_runs_then_it_surfaces_invalid_recipient() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        send_message: Some(Box::new(|_, _| {
            Err(named_error(
                "InvalidRecipientError",
                "unknown or inactive team recipient: ghost",
            ))
        })),
        ..Default::default()
    });

    let result = run_team_send(&service, "run-2", "alpha", &input("ghost", "x")).expect("send");

    let details = serde_json::to_value(&result.details).expect("details");
    assert_eq!(details["kind"], json!("invalid_recipient"));
    assert_eq!(details["to"], json!("ghost"));
}
