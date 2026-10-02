use std::collections::BTreeSet;
use maho_omo_task::{team_service::assert_canonical_team_run_id, team_service_support::{build_member_ports, resolve_team_spec}};
use senpi_task::team::{errors::SenpiTeamSpecErrorCode, registry::TeamSpecSource};
use serde_json::json;

#[test]
fn member_vocabulary_includes_defaults_and_custom_categories() {
    let ports = build_member_ports(&json!({"categories":{"custom":{"disable":true}}}), &BTreeSet::from(["worker".into()]));
    assert!((ports.is_category_resolvable)("quick"));
    assert!((ports.is_category_resolvable)("custom"));
    assert!(!(ports.is_category_resolvable)("absent"));
    assert!((ports.is_known_agent)("worker"));
    assert!(!(ports.is_known_agent)("absent"));
}
#[test]
fn inline_spec_wins_over_named_lookup() {
    let root = tempfile::tempdir().unwrap();
    let ports = build_member_ports(&json!({}), &BTreeSet::new());
    let raw = json!({"name":"inline","members":[{"name":"beta","kind":"category","category":"quick","prompt":"work"}]});
    let resolved = resolve_team_spec(Some("missing"), Some(&raw), &ports, root.path(), None).unwrap();
    assert_eq!(resolved.spec.name, "inline"); assert_eq!(resolved.source, TeamSpecSource::OmoJson);
}
#[test]
fn unnamed_inline_spec_uses_inline_team_name() {
    let root = tempfile::tempdir().unwrap(); let ports = build_member_ports(&json!({}), &BTreeSet::new());
    let raw = json!({"members":[{"kind":"category","category":"quick","prompt":"work"}]});
    assert_eq!(resolve_team_spec(None, Some(&raw), &ports, root.path(), None).unwrap().spec.name, "inline-team");
}
#[test]
fn missing_request_is_invalid_spec() {
    let root = tempfile::tempdir().unwrap(); let ports = build_member_ports(&json!({}), &BTreeSet::new());
    let error = resolve_team_spec(None, None, &ports, root.path(), None).err().unwrap();
    assert_eq!(error.code, SenpiTeamSpecErrorCode::InvalidSpec); assert_eq!(error.team_name, "unknown");
}
#[test]
fn unknown_name_lists_sorted_declared_and_broken_teams() {
    let root = tempfile::tempdir().unwrap(); let ports = build_member_ports(&json!({}), &BTreeSet::new());
    let teams = json!({"z":{"members":[{"kind":"category","category":"quick","prompt":"work"}]},"a":{"members":[{"kind":"category","category":"quick","prompt":"work"}]},"broken":{"members":[]}});
    let error = resolve_team_spec(Some("missing"), None, &ports, root.path(), teams.as_object()).err().unwrap();
    assert!(error.message.contains("Declared teams: a, z.")); assert!(error.message.contains("failed to load: broken"));
}
#[test]
fn known_broken_name_returns_recorded_spec_error() {
    let root = tempfile::tempdir().unwrap(); let ports = build_member_ports(&json!({}), &BTreeSet::new());
    let teams = json!({"broken":{"members":[{"kind":"category","category":"missing"}]}});
    let error = resolve_team_spec(Some("broken"), None, &ports, root.path(), teams.as_object()).err().unwrap();
    assert_eq!(error.code, SenpiTeamSpecErrorCode::InvalidSpec); assert!(error.message.contains("unknown category"));
}
#[test]
fn curated_agent_rejected_before_team_creation() {
    let root = tempfile::tempdir().unwrap(); let ports = build_member_ports(&json!({}), &BTreeSet::from(["momus".into()]));
    let raw = json!({"members":[{"name":"momus","kind":"subagent_type","subagent_type":"momus","prompt":"review"}]});
    let error = resolve_team_spec(None, Some(&raw), &ports, root.path(), None).err().unwrap();
    assert_eq!(error.code, SenpiTeamSpecErrorCode::UnknownSubagentType);
}
#[test]
fn canonical_run_id_accepts_only_lowercase_v4_uuid() {
    assert!(assert_canonical_team_run_id("77777777-7777-4777-8777-777777777777").is_ok());
    for invalid in ["run-1", "77777777-7777-3777-8777-777777777777", "77777777-7777-4777-7777-777777777777", "AAAAAAAA-7777-4777-8777-777777777777"] { assert!(assert_canonical_team_run_id(invalid).is_err()); }
}
