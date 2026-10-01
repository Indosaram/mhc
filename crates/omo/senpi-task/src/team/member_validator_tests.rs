//! `team/member-validator.test.ts`

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use team_core::types::TeamSpec;

use crate::agents::one_shot_agent_names;
use crate::team::errors::{SenpiTeamSpecError, SenpiTeamSpecErrorCode};
use crate::team::member_validator::{SenpiTeamMemberPorts, validate_senpi_team_members};
use crate::team::normalize::normalize_senpi_team_spec;

fn allow_all() -> SenpiTeamMemberPorts {
    SenpiTeamMemberPorts {
        is_category_resolvable: Box::new(|_| true),
        is_known_agent: Box::new(|_| true),
        category_names: None,
        agent_names: None,
    }
}

fn spec(raw: &Value, team_name: &str) -> TeamSpec {
    normalize_senpi_team_spec(raw, team_name, None).expect("normalized spec")
}

fn expect_error(spec: &TeamSpec, ports: &SenpiTeamMemberPorts) -> SenpiTeamSpecError {
    match validate_senpi_team_members(spec, ports) {
        Ok(()) => panic!("expected SenpiTeamSpecError"),
        Err(error) => error,
    }
}

#[test]
fn given_an_unknown_category_with_named_ports_when_validated_then_the_error_lists_the_available_categories() {
    let spec = spec(
        &json!({ "members": [{ "kind": "category", "category": "quikc", "prompt": "work" }] }),
        "typo-team",
    );
    let ports = SenpiTeamMemberPorts {
        is_category_resolvable: Box::new(|category| category == "quick" || category == "deep"),
        is_known_agent: Box::new(|_| true),
        category_names: Some(vec!["deep".to_string(), "quick".to_string()]),
        agent_names: None,
    };

    let error = expect_error(&spec, &ports);

    assert!(error.message.contains("Available categories: deep, quick"));
}

#[test]
fn given_an_unknown_subagent_type_with_named_ports_when_validated_then_the_error_lists_the_available_agents() {
    let spec = spec(
        &json!({ "members": [{ "kind": "subagent_type", "subagent_type": "orakel", "prompt": "work" }] }),
        "typo-team",
    );
    let ports = SenpiTeamMemberPorts {
        is_category_resolvable: Box::new(|_| true),
        is_known_agent: Box::new(|agent| agent == "sisyphus"),
        category_names: None,
        agent_names: Some(vec!["sisyphus".to_string()]),
    };

    let error = expect_error(&spec, &ports);

    assert!(error.message.contains("Available agents: sisyphus"));
}

#[test]
fn given_resolvable_members_when_validated_then_it_passes_without_throwing() {
    let spec = spec(
        &json!({
            "members": [
                { "kind": "category", "category": "quick", "prompt": "work" },
                { "kind": "agent", "subagent_type": "finder" },
            ],
        }),
        "research-team",
    );

    assert!(validate_senpi_team_members(&spec, &allow_all()).is_ok());
}

#[test]
fn given_an_unresolvable_category_when_validated_then_it_throws_naming_the_allowed_kinds() {
    let spec = spec(
        &json!({ "members": [{ "kind": "unresolvable-kind", "name": "x" }] }),
        "bad-category-team",
    );
    let ports = SenpiTeamMemberPorts {
        is_category_resolvable: Box::new(|_| false),
        is_known_agent: Box::new(|_| true),
        category_names: None,
        agent_names: None,
    };

    let error = expect_error(&spec, &ports);

    assert_eq!(error.code, SenpiTeamSpecErrorCode::UnresolvableCategory);
    assert!(error.message.contains("category"));
    assert!(error.message.contains("subagent_type"));
    assert!(error.message.contains("agent"));
}

#[test]
fn given_an_unknown_subagent_type_when_validated_then_it_throws_a_typed_diagnostic() {
    let spec = spec(
        &json!({ "members": [{ "kind": "agent", "subagent_type": "not-loaded" }] }),
        "bad-agent-team",
    );
    let ports = SenpiTeamMemberPorts {
        is_category_resolvable: Box::new(|_| true),
        is_known_agent: Box::new(|_| false),
        category_names: None,
        agent_names: None,
    };

    let error = expect_error(&spec, &ports);

    assert_eq!(error.code, SenpiTeamSpecErrorCode::UnknownSubagentType);
}

#[test]
fn given_a_curated_read_only_agent_when_validated_then_it_is_rejected_before_the_known_agent_check() {
    let spec = spec(
        &json!({ "members": [{ "kind": "agent", "subagent_type": "momus" }] }),
        "curated-agent-team",
    );

    let error = expect_error(&spec, &allow_all());

    assert_eq!(error.code, SenpiTeamSpecErrorCode::UnknownSubagentType);
    assert_eq!(
        error.message,
        "curated read-only agent \"momus\" cannot be a team member; delegate via the task tool instead"
    );
}

#[test]
fn given_a_one_shot_agent_name_when_used_as_a_subagent_type_then_it_throws_senpi_team_spec_error() {
    // For EVERY name in the one-shot registry, a member spec using that subagent_type must be
    // rejected. This ties the one-shot registry to the team-member rejection path so the guarantee
    // survives future roster changes.
    let names = one_shot_agent_names();
    assert!(!names.is_empty());

    for agent_name in names {
        let spec = spec(
            &json!({ "members": [{ "kind": "agent", "subagent_type": agent_name }] }),
            "one-shot-invariant-team",
        );

        let error = expect_error(&spec, &allow_all());

        assert_eq!(error.code, SenpiTeamSpecErrorCode::UnknownSubagentType);
    }
}
