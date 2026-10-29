use super::argument_normalization::normalize_task_tool_arguments;
use crate::kernel_tools::{KernelToolErrorCode, KernelToolGrantResolution, KernelToolGrantRequest, KernelToolsCapability, resolve_kernel_tool_grant};
use crate::manager::execution_mode::ExecutionMode;
use crate::tools::task::validation::{BatchShape, SpawnParamsInput, resolve_spawn_items, validate_batch_shape};
use serde_json::json;

#[test]
fn requested_tool_names_survive_normalization() {
    let value = normalize_task_tool_arguments(&json!({"prompt":"p","category":"quick","tools":["lookup"," ",7]})).unwrap();
    assert_eq!(value["tools"], json!(["lookup"]));
    assert!(normalize_task_tool_arguments(&json!({"prompt":"p","category":"quick"})).unwrap().get("tools").is_none());
}

#[test]
fn live_parent_descriptor_is_granted_to_child() {
    let request = KernelToolGrantRequest { requested_names: vec!["lookup".into()], capability: Some(KernelToolsCapability::default()), execution_mode: ExecutionMode::InProcess, agent_type: None, team_role_member: false, existing_tool_names: vec![], tool_allowlist: None, tool_denylist: None };
    let described = json!({"results":[{"name":"lookup","ok":true,"descriptor":{"name":"lookup","description":"fixture","input_schema":{},"language":"js","kernel_generation":1,"definition_revision":1}}]});
    let KernelToolGrantResolution::Granted(grant) = resolve_kernel_tool_grant(&request, Some(&described)) else { panic!("expected grant") };
    assert_eq!(grant.descriptors.len(), 1);
    assert_eq!(grant.descriptors[0].name, "lookup");
}

#[test]
fn missing_tool_is_typed_as_kernel_tool_missing() {
    let request = KernelToolGrantRequest { requested_names: vec!["missing".into()], capability: Some(KernelToolsCapability::default()), execution_mode: ExecutionMode::InProcess, agent_type: None, team_role_member: false, existing_tool_names: vec![], tool_allowlist: None, tool_denylist: None };
    let described = json!({"results":[{"name":"missing","ok":false,"error":{"code":"kernel_tool_missing","message":"missing"}}]});
    assert!(matches!(resolve_kernel_tool_grant(&request, Some(&described)), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::KernelToolMissing, .. }));
}

#[test]
fn process_mode_refuses_kernel_tool_grant() {
    let request = KernelToolGrantRequest { requested_names: vec!["lookup".into()], capability: Some(KernelToolsCapability::default()), execution_mode: ExecutionMode::Process, agent_type: None, team_role_member: false, existing_tool_names: vec![], tool_allowlist: None, tool_denylist: None };
    assert!(matches!(resolve_kernel_tool_grant(&request, None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ToolsUnavailable, .. }));
}

#[test]
fn missing_parent_capability_refuses_grant() {
    let request = KernelToolGrantRequest { requested_names: vec!["lookup".into()], capability: None, execution_mode: ExecutionMode::InProcess, agent_type: None, team_role_member: false, existing_tool_names: vec![], tool_allowlist: None, tool_denylist: None };
    assert!(matches!(resolve_kernel_tool_grant(&request, None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ToolsUnavailable, .. }));
}

#[test]
fn curated_agent_refuses_grant() {
    let request = KernelToolGrantRequest { requested_names: vec!["lookup".into()], capability: Some(KernelToolsCapability::default()), execution_mode: ExecutionMode::InProcess, agent_type: Some("explore".into()), team_role_member: false, existing_tool_names: vec![], tool_allowlist: None, tool_denylist: None };
    assert!(matches!(resolve_kernel_tool_grant(&request, None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::CuratedPolicyDenied, .. }));
}

#[test]
fn batch_with_missing_tool_has_no_resolved_spawn_items() {
    let params = SpawnParamsInput { category: Some("quick".into()), tasks: Some(vec![]), ..SpawnParamsInput::default() };
    assert!(matches!(validate_batch_shape(&params), BatchShape::Error(_)));
    assert!(resolve_spawn_items(&params).is_err());
}

#[test]
fn named_grant_preserves_requested_identity() {
    let request = KernelToolGrantRequest { requested_names: vec!["lookup".into()], capability: Some(KernelToolsCapability::default()), execution_mode: ExecutionMode::InProcess, agent_type: None, team_role_member: false, existing_tool_names: vec![], tool_allowlist: None, tool_denylist: None };
    let described = json!({"results":[{"name":"lookup","ok":true,"descriptor":{"name":"lookup","description":"fixture","input_schema":{},"language":"js","kernel_generation":1,"definition_revision":1}}]});
    let KernelToolGrantResolution::Granted(grant) = resolve_kernel_tool_grant(&request, Some(&described)) else { panic!("expected grant") };
    assert_eq!(grant.requested_names, vec!["lookup"]);
}
