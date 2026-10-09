use super::*;
use crate::manager::execution_mode::ExecutionMode;
use serde_json::json;

fn request(capability: KernelToolsCapability) -> KernelToolGrantRequest {
    KernelToolGrantRequest {
        requested_names: vec!["fixture_write".into()],
        capability: Some(capability),
        execution_mode: ExecutionMode::InProcess,
        agent_type: None,
        team_role_member: false,
        existing_tool_names: vec!["read".into(), "grep".into()],
        tool_allowlist: None,
        tool_denylist: None,
    }
}

fn described() -> serde_json::Value {
    json!({"results":[{"name":"fixture_write","ok":true,"descriptor":{"name":"fixture_write","description":"fixture","input_schema":{},"language":"js","kernel_generation":1,"definition_revision":1}}]})
}

#[test]
fn invoke_scope_grants_empty_allowlist_with_deny() {
    let mut r = request(KernelToolsCapability { capabilities: Some(KernelToolCapabilities { invoke_scope: true }) });
    r.tool_allowlist = Some(vec![]);
    r.tool_denylist = Some(vec!["write".into()]);
    let KernelToolGrantResolution::Granted(grant) = resolve_kernel_tool_grant(&r, Some(&described())) else { panic!("expected grant") };
    assert_eq!(grant.requested_names, vec!["fixture_write"]);
    assert_eq!(grant.scope.unwrap().tools.unwrap().allow, Some(vec![]));
}

#[test]
fn invoke_scope_grant_carries_child_effective_policy() {
    let mut r = request(KernelToolsCapability { capabilities: Some(KernelToolCapabilities { invoke_scope: true }) });
    r.tool_denylist = Some(vec!["write".into()]);
    let KernelToolGrantResolution::Granted(grant) = resolve_kernel_tool_grant(&r, Some(&described())) else { panic!("expected grant") };
    let scope = grant.scope.unwrap().tools.unwrap();
    assert_eq!(scope.allow, Some(vec!["read", "bash", "edit", "grep"] .into_iter().map(String::from).collect()));
    assert_eq!(scope.deny, Some(vec!["write".into()]));
}

#[test]
fn curated_agent_refuses_parent_tool_before_describe() {
    let mut r = request(KernelToolsCapability { capabilities: Some(KernelToolCapabilities { invoke_scope: true }) });
    r.agent_type = Some("explore".into());
    assert!(matches!(resolve_kernel_tool_grant(&r, None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::CuratedPolicyDenied, .. }));
}
