use super::*;
use crate::manager::execution_mode::ExecutionMode;
use serde_json::json;

fn request(names: &[&str]) -> KernelToolGrantRequest {
    KernelToolGrantRequest {
        requested_names: names.iter().map(|name| (*name).into()).collect(),
        capability: Some(KernelToolsCapability::default()),
        execution_mode: ExecutionMode::InProcess,
        agent_type: None,
        team_role_member: false,
        existing_tool_names: Vec::new(),
        tool_allowlist: None,
        tool_denylist: None,
    }
}

fn described(names: &[&str]) -> serde_json::Value {
    json!({"results": names.iter().map(|name| json!({"name":name,"ok":true,"descriptor":{"name":name,"description":"fixture","input_schema":{},"language":"js","kernel_generation":1,"definition_revision":1}})).collect::<Vec<_>>()})
}

#[test]
fn normalize_names_uses_mcp_grammar_and_collision_key() {
    assert_eq!(normalize_kernel_tool_name("look up!"), "look_up_");
    assert_eq!(normalize_kernel_tool_name(&"a".repeat(70)).len(), 64);
    assert_eq!(kernel_tool_key("look-up"), kernel_tool_key("look_up"));
}

#[test]
fn bridge_and_task_family_names_are_reserved() {
    for name in ["__agent__", "task", "task_send", "team_create", "workpool"] {
        assert!(is_reserved_kernel_tool_name(name));
    }
    assert!(!is_reserved_kernel_tool_name("lookup"));
}

#[test]
fn live_parent_descriptors_are_granted() {
    let result = resolve_kernel_tool_grant(&request(&["lookup"]), Some(&described(&["lookup"])));
    assert!(matches!(result, KernelToolGrantResolution::Granted(grant) if grant.descriptors.len() == 1));
}

#[test]
fn empty_requests_do_not_describe() {
    assert!(matches!(resolve_kernel_tool_grant(&request(&[]), None), KernelToolGrantResolution::None));
}

#[test]
fn curated_agents_are_denied() {
    let mut r = request(&["lookup"]);
    r.agent_type = Some("explore".into());
    assert!(matches!(resolve_kernel_tool_grant(&r, None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::CuratedPolicyDenied, .. }));
}

#[test]
fn process_team_and_missing_parent_are_unavailable() {
    let mut process = request(&["lookup"]);
    process.execution_mode = ExecutionMode::Process;
    assert!(matches!(resolve_kernel_tool_grant(&process, None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ToolsUnavailable, .. }));
    let mut team = request(&["lookup"]);
    team.team_role_member = true;
    assert!(matches!(resolve_kernel_tool_grant(&team, None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ToolsUnavailable, .. }));
    let mut missing = request(&["lookup"]);
    missing.capability = None;
    assert!(matches!(resolve_kernel_tool_grant(&missing, None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ToolsUnavailable, .. }));
}

#[test]
fn duplicate_names_are_rejected() {
    assert!(matches!(resolve_kernel_tool_grant(&request(&["lookup", "lookup"]), None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ToolNameCollision, .. }));
    assert!(matches!(resolve_kernel_tool_grant(&request(&["look_up", "look-up"]), None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ToolNameCollision, .. }));
}

#[test]
fn existing_child_name_collision_is_rejected() {
    let mut r = request(&["grep"]);
    r.existing_tool_names = vec!["grep".into(), "read".into()];
    assert!(matches!(resolve_kernel_tool_grant(&r, None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ToolNameCollision, .. }));
}

#[test]
fn reserved_alias_is_rejected_before_describe() {
    assert!(matches!(resolve_kernel_tool_grant(&request(&["task_send"]), None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ReservedToolName, .. }));
}

#[test]
fn missing_descriptor_is_typed() {
    let result = resolve_kernel_tool_grant(&request(&["missing"]), Some(&json!({"results":[{"name":"missing","ok":false,"error":{"code":"kernel_tool_missing","message":"missing"}}]})));
    assert!(matches!(result, KernelToolGrantResolution::Denied { code: KernelToolErrorCode::KernelToolMissing, .. }));
}

#[test]
fn narrowed_write_policy_fails_closed() {
    let mut r = request(&["lookup"]);
    r.existing_tool_names = ["read", "grep", "write", "edit", "bash", "web_search"].into_iter().map(String::from).collect();
    r.tool_denylist = Some(vec!["write".into()]);
    assert!(matches!(resolve_kernel_tool_grant(&r, Some(&described(&["lookup"]))), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ToolsUnavailable, .. }));
}

#[test]
fn dead_parent_is_unavailable() {
    let mut r = request(&["lookup"]);
    r.capability = None;
    assert!(matches!(resolve_kernel_tool_grant(&r, None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ToolsUnavailable, .. }));
}

#[test]
fn granted_descriptor_preserves_requested_name() {
    let result = resolve_kernel_tool_grant(&request(&["lookup"]), Some(&described(&["lookup"])));
    let KernelToolGrantResolution::Granted(grant) = result else { panic!("expected grant") };
    assert_eq!(grant.requested_names, ["lookup"]);
}


#[test]
fn invalid_descriptor_language_is_not_accepted() {
    let descriptor = json!({"name":"lookup","description":"fixture","input_schema":{},"language":"rb","kernel_generation":1,"definition_revision":1});
    assert!(!is_kernel_tool_descriptor(&descriptor));
}

#[test]
fn failed_wrapper_result_is_typed_for_child_channel() {
    let result = kernel_tool_error_result("lookup", KernelToolErrorCode::KernelToolFailed, "closure exploded");
    assert_eq!(result["isError"], true);
    assert_eq!(result["details"]["error"]["code"], "kernel_tool_failed");
}
