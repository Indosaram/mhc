//! `team/normalize.test.ts`

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::team::errors::{SenpiTeamSpecError, SenpiTeamSpecErrorCode};
use crate::team::normalize::{NormalizeSenpiTeamSpecOptions, TEAM_LEAD_SENTINEL, normalize_senpi_team_spec};

fn expect_error(
    raw: &Value,
    team_name: &str,
    options: Option<&NormalizeSenpiTeamSpecOptions>,
) -> SenpiTeamSpecError {
    match normalize_senpi_team_spec(raw, team_name, options) {
        Ok(_) => panic!("expected SenpiTeamSpecError"),
        Err(error) => error,
    }
}

#[test]
fn given_a_multi_member_spec_with_a_category_and_an_agent_alias_when_normalized_then_it_parses_with_the_lead_sentinel() {
    let raw = json!({
        "members": [
            { "kind": "category", "category": "quick", "prompt": "investigate the failing test" },
            { "kind": "agent", "subagent_type": "finder" },
        ],
    });

    let spec = normalize_senpi_team_spec(&raw, "research-team", None).expect("spec");

    assert_eq!(spec.name, "research-team");
    assert_eq!(spec.lead_agent_id.as_str(), TEAM_LEAD_SENTINEL);
    assert_eq!(spec.members.len(), 2);
    assert!(!spec.members.iter().any(|member| member.name == TEAM_LEAD_SENTINEL));
    assert_eq!(spec.members[0].kind.as_str(), "category");
    assert_eq!(spec.members[1].kind.as_str(), "subagent_type");
    assert_eq!(spec.members[1].subagent_type.as_deref(), Some("finder"));
}

#[test]
fn given_a_name_less_omo_json_team_value_when_normalized_then_it_takes_the_record_key_as_its_name() {
    let raw = json!({ "members": [{ "kind": "subagent_type", "subagent_type": "sisyphus" }] });

    let spec = normalize_senpi_team_spec(&raw, "solo-team", None).expect("spec");

    assert_eq!(spec.name, "solo-team");
    assert_eq!(spec.lead_agent_id.as_str(), TEAM_LEAD_SENTINEL);
}

#[test]
fn given_a_spec_that_already_carries_its_own_name_when_normalized_then_the_explicit_name_is_preserved() {
    let raw = json!({ "name": "explicit-name", "members": [{ "kind": "subagent_type", "subagent_type": "atlas" }] });

    let spec = normalize_senpi_team_spec(&raw, "record-key", None).expect("spec");

    assert_eq!(spec.name, "explicit-name");
}

#[test]
fn given_a_raw_lead_field_on_the_input_when_normalized_then_it_is_rejected_with_a_typed_diagnostic() {
    let raw = json!({
        "lead": { "kind": "subagent_type", "subagent_type": "sisyphus" },
        "members": [{ "kind": "category", "category": "quick", "prompt": "work" }],
    });

    let error = expect_error(&raw, "with-raw-lead", None);

    assert_eq!(error.code, SenpiTeamSpecErrorCode::ReservedLeadField);
    assert_eq!(error.name(), "SenpiTeamSpecError");
}

#[test]
fn given_a_member_literally_named_lead_when_normalized_then_it_is_rejected_with_a_typed_diagnostic() {
    let raw = json!({
        "members": [
            { "kind": "subagent_type", "subagent_type": "sisyphus", "name": "lead" },
            { "kind": "category", "category": "quick", "prompt": "work" },
        ],
    });

    let error = expect_error(&raw, "with-lead-member", None);

    assert_eq!(error.code, SenpiTeamSpecErrorCode::ReservedLeadMember);
}

#[test]
fn given_the_caller_team_lead_option_when_normalized_then_it_is_rejected_before_any_member_is_spawned() {
    let raw = json!({ "members": [{ "kind": "category", "category": "quick", "prompt": "work" }] });
    let options = NormalizeSenpiTeamSpecOptions {
        caller_team_lead: Some(json!({ "agentTypeId": "sisyphus" })),
    };

    let error = expect_error(&raw, "with-caller-lead", Some(&options));

    assert_eq!(error.code, SenpiTeamSpecErrorCode::ReservedCallerTeamLead);
}

#[test]
fn given_a_member_with_no_resolvable_kind_fields_when_normalized_then_the_schema_rejects_it_as_an_invalid_spec() {
    let raw = json!({ "members": [{ "kind": "agent" }] });

    let error = expect_error(&raw, "broken-agent", None);

    assert_eq!(error.code, SenpiTeamSpecErrorCode::InvalidSpec);
}

#[test]
fn given_a_json_stringified_spec_when_normalized_then_it_parses_and_normalizes_like_the_object_form() {
    let payload = json!({ "members": [{ "kind": "category", "category": "quick", "prompt": "work" }] }).to_string();

    let spec = normalize_senpi_team_spec(&Value::String(payload), "string-team", None).expect("spec");

    assert_eq!(spec.name, "string-team");
    assert_eq!(spec.lead_agent_id.as_str(), TEAM_LEAD_SENTINEL);
    assert_eq!(spec.members.len(), 1);
}

#[test]
fn given_a_malformed_json_string_spec_when_normalized_then_it_rejects_with_the_parse_detail_and_the_corrective_shape() {
    let error = expect_error(&json!("{not json"), "bad-string", None);

    assert_eq!(error.code, SenpiTeamSpecErrorCode::InvalidSpec);
    assert!(error.message.contains("JSON"));
    assert!(error.message.contains("object"));
}

#[test]
fn given_a_non_string_non_object_spec_when_normalized_then_the_error_names_the_received_type_and_the_corrective_shape() {
    let error = expect_error(&json!(42), "numeric", None);

    assert_eq!(error.code, SenpiTeamSpecErrorCode::InvalidSpec);
    assert!(error.message.contains("number"));
    assert!(error.message.contains("object"));
}

#[test]
fn given_a_single_member_object_instead_of_an_array_when_normalized_then_it_is_wrapped_into_a_one_member_array() {
    let raw = json!({ "members": { "kind": "category", "category": "quick", "prompt": "work" } });

    let spec = normalize_senpi_team_spec(&raw, "single-member", None).expect("spec");

    assert_eq!(spec.members.len(), 1);
    assert_eq!(spec.members[0].kind.as_str(), "category");
}

#[test]
fn given_a_member_with_a_task_summary_when_normalized_then_the_summary_survives_the_schema_parse() {
    let raw = json!({
        "members": [{ "kind": "category", "category": "quick", "prompt": "work", "task_summary": "Investigate the failing test" }],
    });

    let spec = normalize_senpi_team_spec(&raw, "demo", None).expect("spec");

    assert_eq!(spec.members[0].task_summary.as_deref(), Some("Investigate the failing test"));
}

#[test]
fn given_an_over_limit_member_task_summary_when_normalized_then_it_is_clamped_instead_of_rejected() {
    let raw = json!({
        "members": [{ "kind": "category", "category": "quick", "prompt": "work", "task_summary": "s".repeat(200) }],
    });

    let spec = normalize_senpi_team_spec(&raw, "demo", None).expect("spec");

    let summary = spec.members[0].task_summary.clone().expect("summary");
    assert_eq!(summary.chars().count(), 80);
    assert!(summary.ends_with("..."));
}
