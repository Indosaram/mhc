//! `team/messaging/message.test.ts`

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::team::messaging::message::{
    BuildTeamMessageOptions, build_peer_message_envelope, build_team_message,
};
use crate::team::messaging::types::SendTeamMessageInput;

fn input(from: &str, to: &str, body: &str, summary: Option<&str>) -> SendTeamMessageInput {
    SendTeamMessageInput {
        from: from.to_string(),
        to: to.to_string(),
        body: body.to_string(),
        summary: summary.map(str::to_string),
    }
}

#[test]
fn build_team_message_given_from_to_body_when_built_then_a_well_formed_message_kind_message_is_produced()
{
    // given
    let now = || 1_700_000_000_000_i64;
    let new_message_id = || "11111111-1111-4111-8111-111111111111".to_string();
    let options = BuildTeamMessageOptions {
        now: Some(&now),
        new_message_id: Some(&new_message_id),
    };

    // when
    let message = build_team_message(&input("alpha", "beta", "ping", None), &options).unwrap();

    // then
    let value = serde_json::to_value(&message).unwrap();
    assert_eq!(
        value,
        json!({
            "version": 1,
            "messageId": "11111111-1111-4111-8111-111111111111",
            "from": "alpha",
            "to": "beta",
            "kind": "message",
            "body": "ping",
            "timestamp": 1_700_000_000_000_i64,
        })
    );
}

#[test]
fn build_team_message_given_a_summary_when_built_then_the_summary_is_carried_and_a_broadcast_target_is_preserved()
 {
    // given / when
    let now = || 1_i64;
    let new_message_id = || "22222222-2222-4222-8222-222222222222".to_string();
    let options = BuildTeamMessageOptions {
        now: Some(&now),
        new_message_id: Some(&new_message_id),
    };
    let message =
        build_team_message(&input("alpha", "*", "hi", Some("greeting")), &options).unwrap();

    // then
    let value = serde_json::to_value(&message).unwrap();
    assert_eq!(value.get("summary"), Some(&Value::String("greeting".to_string())));
    assert_eq!(value.get("to"), Some(&Value::String("*".to_string())));
}

#[test]
fn build_peer_message_envelope_given_a_message_with_markup_bearing_fields_when_the_envelope_is_built_then_attributes_are_escaped_in_team_cores_order()
 {
    // given
    let now = || 42_i64;
    let new_message_id = || "33333333-3333-4333-8333-333333333333".to_string();
    let options = BuildTeamMessageOptions {
        now: Some(&now),
        new_message_id: Some(&new_message_id),
    };
    let message =
        build_team_message(&input("al<pha", "beta", "body & <content>", None), &options).unwrap();

    // when
    let envelope = build_peer_message_envelope(&message);

    // then
    assert_eq!(
        envelope,
        "<peer_message from=\"al&lt;pha\" timestamp=\"42\" messageId=\"33333333-3333-4333-8333-333333333333\" kind=\"message\" correlationId=\"\">\nbody & <content>\n</peer_message>"
    );
}

#[test]
fn build_peer_message_envelope_given_a_summary_when_the_envelope_is_built_then_the_summary_attribute_is_appended()
 {
    // given
    let now = || 7_i64;
    let new_message_id = || "44444444-4444-4444-8444-444444444444".to_string();
    let options = BuildTeamMessageOptions {
        now: Some(&now),
        new_message_id: Some(&new_message_id),
    };
    let message =
        build_team_message(&input("alpha", "beta", "b", Some("the-summary")), &options).unwrap();

    // when
    let envelope = build_peer_message_envelope(&message);

    // then
    assert!(
        envelope.contains("summary=\"the-summary\""),
        "envelope was: {envelope}"
    );
}
