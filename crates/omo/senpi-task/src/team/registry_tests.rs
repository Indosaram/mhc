//! `team/registry.test.ts`

use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::team::member_validator::SenpiTeamMemberPorts;
use crate::team::registry::{LoadTeamRegistryInput, TeamSpecSource, load_team_registry};

fn allow_all() -> SenpiTeamMemberPorts {
    SenpiTeamMemberPorts {
        is_category_resolvable: Box::new(|_| true),
        is_known_agent: Box::new(|_| true),
        category_names: None,
        agent_names: None,
    }
}

fn deny_all() -> SenpiTeamMemberPorts {
    SenpiTeamMemberPorts {
        is_category_resolvable: Box::new(|_| false),
        is_known_agent: Box::new(|_| false),
        category_names: None,
        agent_names: None,
    }
}

fn to_map(value: Value) -> Map<String, Value> {
    value.as_object().cloned().expect("object")
}

fn write_project_team_spec(project_root: &Path, team_name: &str, spec: &Value) {
    let team_dir = project_root.join(".omo").join("teams").join(team_name);
    fs::create_dir_all(&team_dir).expect("create team dir");
    fs::write(
        team_dir.join("config.json"),
        serde_json::to_string(spec).expect("serialize spec"),
    )
    .expect("write spec");
}

#[test]
fn given_omo_json_teams_with_category_and_agent_alias_member_when_loaded_then_round_trips_into_team_core_spec() {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    let omo_teams = to_map(json!({
        "research-team": {
            "members": [
                { "kind": "category", "category": "quick", "prompt": "investigate" },
                { "kind": "agent", "subagent_type": "finder" },
            ],
        },
    }));
    let ports = allow_all();

    // when
    let result = load_team_registry(&LoadTeamRegistryInput {
        project_root: project.path(),
        omo_teams: Some(&omo_teams),
        ports: &ports,
    });

    // then
    assert_eq!(result.errors, Vec::new());
    assert_eq!(result.teams.len(), 1);
    let entry = &result.teams[0];
    assert_eq!(entry.name, "research-team");
    assert_eq!(entry.source, TeamSpecSource::OmoJson);
    assert_eq!(entry.source.as_str(), "omo-json");
    let spec_json = serde_json::to_value(&entry.spec).expect("serialize spec");
    assert_eq!(spec_json.get("leadAgentId").and_then(Value::as_str), Some("lead"));
    assert_eq!(entry.spec.members.len(), 2);
}

#[test]
fn given_directory_spec_and_omo_json_spec_sharing_a_name_when_loaded_then_project_directory_wins() {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    write_project_team_spec(
        project.path(),
        "shared",
        &json!({ "members": [{ "kind": "subagent_type", "subagent_type": "atlas" }] }),
    );
    let omo_teams = to_map(json!({
        "shared": { "members": [{ "kind": "category", "category": "quick", "prompt": "work" }] },
    }));
    let ports = allow_all();

    // when
    let result = load_team_registry(&LoadTeamRegistryInput {
        project_root: project.path(),
        omo_teams: Some(&omo_teams),
        ports: &ports,
    });

    // then
    assert_eq!(result.teams.len(), 1);
    let entry = &result.teams[0];
    assert_eq!(entry.name, "shared");
    assert_eq!(entry.source, TeamSpecSource::Project);
    assert_eq!(entry.source.as_str(), "project");
    assert_eq!(entry.spec.members[0].kind.as_str(), "subagent_type");
}

#[test]
fn given_member_with_unresolvable_kind_when_loaded_then_error_recorded_and_zero_teams_spawn() {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    let omo_teams = to_map(json!({
        "bad-team": { "members": [{ "kind": "unresolvable-kind", "name": "x" }] },
    }));
    let ports = deny_all();

    // when
    let result = load_team_registry(&LoadTeamRegistryInput {
        project_root: project.path(),
        omo_teams: Some(&omo_teams),
        ports: &ports,
    });

    // then
    assert!(result.teams.is_empty());
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].name, "bad-team");
    assert_eq!(result.errors[0].code, "UNRESOLVABLE_CATEGORY");
}

#[test]
fn given_team_declaring_raw_lead_field_when_loaded_then_rejected_and_spawns_zero_members() {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    let omo_teams = to_map(json!({
        "lead-field-team": {
            "lead": { "kind": "subagent_type", "subagent_type": "sisyphus" },
            "members": [{ "kind": "category", "category": "quick", "prompt": "work" }],
        },
    }));
    let ports = allow_all();

    // when
    let result = load_team_registry(&LoadTeamRegistryInput {
        project_root: project.path(),
        omo_teams: Some(&omo_teams),
        ports: &ports,
    });

    // then
    assert!(result.teams.is_empty());
    assert_eq!(result.errors[0].code, "RESERVED_LEAD_FIELD");
}

#[test]
fn given_no_team_sources_when_loaded_then_returns_empty_teams_and_errors() {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    let ports = allow_all();

    // when
    let result = load_team_registry(&LoadTeamRegistryInput {
        project_root: project.path(),
        omo_teams: None,
        ports: &ports,
    });

    // then
    assert!(result.teams.is_empty());
    assert_eq!(result.errors, Vec::new());
}
