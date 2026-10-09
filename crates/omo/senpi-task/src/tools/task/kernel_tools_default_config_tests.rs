use crate::agents::{BUILTIN_AGENT_DEFAULTS, curated_readonly_agent_names};
use crate::category::BUILTIN_CATEGORY_DEFAULTS;
use crate::kernel_tools::{KernelToolErrorCode, KernelToolGrantResolution, KernelToolGrantRequest, KernelToolsCapability, resolve_kernel_tool_grant};
use crate::manager::execution_mode::ExecutionMode;

fn request(target_agent: Option<&str>) -> KernelToolGrantRequest {
    KernelToolGrantRequest {
        requested_names: vec!["lookup".into()],
        capability: Some(KernelToolsCapability::default()),
        execution_mode: ExecutionMode::InProcess,
        agent_type: target_agent.map(str::to_owned),
        team_role_member: false,
        existing_tool_names: vec!["read".into(), "grep".into(), "find".into(), "ls".into(), "write".into(), "edit".into(), "bash".into(), "web_search".into(), "lsp_symbols".into()],
        tool_allowlist: None,
        tool_denylist: None,
    }
}

#[test]
fn default_category_children_keep_parent_kernel_tool_grants() {
    assert!(BUILTIN_CATEGORY_DEFAULTS.iter().any(|category| category.name == "quick"));
    assert!(BUILTIN_CATEGORY_DEFAULTS.iter().any(|category| category.name == "unspecified-high"));
    let result = resolve_kernel_tool_grant(&request(None), Some(&serde_json::json!({"results":[]})));
    assert!(matches!(result, KernelToolGrantResolution::Granted(grant) if grant.requested_names == ["lookup"]));
}

#[test]
fn every_builtin_category_has_a_real_definition() {
    assert_eq!(BUILTIN_CATEGORY_DEFAULTS.len(), 9);
    assert!(BUILTIN_CATEGORY_DEFAULTS.iter().all(|category| !category.name.is_empty() && !category.config.model.is_empty()));
}

#[test]
fn builtin_curated_agents_are_refused_by_policy() {
    assert_eq!(curated_readonly_agent_names().len(), BUILTIN_AGENT_DEFAULTS.len());
    for agent in BUILTIN_AGENT_DEFAULTS.iter() {
        let result = resolve_kernel_tool_grant(&request(Some(&agent.name)), None);
        assert!(matches!(result, KernelToolGrantResolution::Denied { code: KernelToolErrorCode::CuratedPolicyDenied, .. }), "{}", agent.name);
    }
}
