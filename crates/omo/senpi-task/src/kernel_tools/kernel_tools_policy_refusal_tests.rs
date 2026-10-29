use super::*;
use crate::manager::execution_mode::ExecutionMode;

fn request() -> KernelToolGrantRequest {
    KernelToolGrantRequest {
        requested_names: vec!["fixture_write".into()],
        capability: Some(KernelToolsCapability::default()),
        execution_mode: ExecutionMode::InProcess,
        agent_type: None,
        team_role_member: false,
        existing_tool_names: vec!["read".into(), "grep".into(), "x_search".into()],
        tool_allowlist: None,
        tool_denylist: None,
    }
}

#[test]
fn empty_allowlist_and_write_deny_fail_closed_without_scope_support() {
    let mut r = request();
    r.tool_allowlist = Some(vec![]);
    r.tool_denylist = Some(vec!["write".into()]);
    assert!(matches!(resolve_kernel_tool_grant(&r, None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ToolsUnavailable, message } if message.contains("write")));
}

#[test]
fn deny_only_write_policy_fails_closed_without_scope_support() {
    let mut r = request();
    r.tool_denylist = Some(vec!["write".into()]);
    assert!(matches!(resolve_kernel_tool_grant(&r, None), KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ToolsUnavailable, message } if message.contains("write")));
}
