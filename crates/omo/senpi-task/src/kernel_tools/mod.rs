//! Structural kernel-tool capability support.

pub mod bindings;
pub mod contract;
pub mod names;
pub mod nested_host_scope;
pub mod resolve;
pub mod transcript_names;
pub mod wrapper;

pub use bindings::*;
pub use contract::*;
pub use names::*;
pub use nested_host_scope::*;
pub use resolve::*;
pub use transcript_names::*;
pub use wrapper::*;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_sanitize_normalize_and_key() {
        assert_eq!(sanitize_kernel_tool_name_part("a b/c"), "a_b_c");
        assert_eq!(kernel_tool_key("a-b"), "a_b");
        assert!(is_reserved_kernel_tool_name("monitor"));
    }

    #[test]
    fn contract_validates_descriptors_and_replies() {
        let descriptor = json!({"name":"fixture","description":"","input_schema":{},"language":"js","kernel_generation":1,"definition_revision":2});
        assert!(is_kernel_tool_descriptor(&descriptor));
        let entries = parse_describe_results(&json!({"results":[{"name":"fixture","ok":true,"descriptor":descriptor}]})).expect("valid reply");
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn nested_scope_applies_allow_and_deny() {
        let request = NestedHostScopeRequest { child_tool_names: Some(vec!["read".into(), "write".into()]), tool_allowlist: Some(vec!["read".into()]), tool_denylist: None, include_task_tools: false };
        assert_eq!(child_effective_tool_names(&request), vec!["read"]);
        assert!(escalating_host_tools(&request).contains(&"write".to_string()));
    }

    #[test]
    fn bindings_replace_release_and_clear() {
        let mut bindings = create_kernel_tool_bindings();
        let capability = resolve::KernelToolsCapability { capabilities: None };
        bindings.bind("child", KernelToolGrant { capability, descriptors: vec![], requested_names: vec![], scope: None });
        assert_eq!(bindings.len(), 1);
        bindings.release("child");
        assert!(bindings.is_empty());
    }
}

#[cfg(test)]
mod invoke_scope_tests {
    use super::*;
    use crate::manager::execution_mode::ExecutionMode;
    use serde_json::json;

    fn request(capability: Option<KernelToolsCapability>) -> KernelToolGrantRequest {
        KernelToolGrantRequest {
            requested_names: vec!["fixture_write".into()],
            capability,
            execution_mode: ExecutionMode::InProcess,
            agent_type: None,
            team_role_member: false,
            existing_tool_names: vec!["read".into(), "grep".into(), "x_search".into()],
            tool_allowlist: None,
            tool_denylist: None,
        }
    }

    #[test]
    fn capability_detection_requires_literal_true_marker() {
        assert!(!supports_invoke_scope(None));
        assert!(!supports_invoke_scope(Some(&KernelToolCapabilities { invoke_scope: false })));
        assert!(supports_invoke_scope(Some(&KernelToolCapabilities { invoke_scope: true })));
    }

    #[test]
    fn child_scope_contains_effective_allow_and_deny() {
        let scope = child_invoke_scope(&NestedHostScopeRequest {
            child_tool_names: Some(vec!["read".into(), "grep".into(), "x_search".into()]),
            tool_allowlist: None,
            tool_denylist: Some(vec!["write".into()]),
            include_task_tools: false,
        });
        assert_eq!(scope.tools.as_ref().and_then(|v| v.allow.clone()), Some(vec!["read".into(), "bash".into(), "edit".into(), "grep".into(), "x_search".into()]));
        assert_eq!(scope.tools.as_ref().and_then(|v| v.deny.clone()), Some(vec!["write".into()]));
    }

    #[test]
    fn invoke_scope_capability_grants_narrowed_scope() {
        let resolution = resolve_kernel_tool_grant(
            &KernelToolGrantRequest { tool_denylist: Some(vec!["write".into()]), capability: Some(KernelToolsCapability { capabilities: Some(KernelToolCapabilities { invoke_scope: true }) }), ..request(None) },
            Some(&json!({"results":[{"name":"fixture_write","ok":true,"descriptor":{"name":"fixture_write","description":"","input_schema":{},"language":"js","kernel_generation":1,"definition_revision":1}}]})),
        );
        let KernelToolGrantResolution::Granted(grant) = resolution else { panic!("expected grant") };
        assert_eq!(grant.scope.as_ref().and_then(|s| s.tools.as_ref()).and_then(|s| s.deny.clone()), Some(vec!["write".into()]));
    }

    #[test]
    fn curated_read_only_agent_is_denied() {
        let resolution = resolve_kernel_tool_grant(
            &KernelToolGrantRequest { agent_type: Some("explore".into()), capability: Some(KernelToolsCapability { capabilities: Some(KernelToolCapabilities { invoke_scope: true }) }), ..request(None) },
            None,
        );
        assert!(matches!(resolution, KernelToolGrantResolution::Denied { code: KernelToolErrorCode::CuratedPolicyDenied, .. }));
    }

    #[test]
    fn missing_capability_refuses_narrowed_child() {
        let resolution = resolve_kernel_tool_grant(
            &KernelToolGrantRequest { tool_denylist: Some(vec!["write".into()]), ..request(None) },
            None,
        );
        assert!(matches!(resolution, KernelToolGrantResolution::Denied { code: KernelToolErrorCode::ToolsUnavailable, message } if message.contains("write")));
    }

    #[test]
    fn missing_capability_keeps_unnarrowed_grant_without_scope() {
        let resolution = resolve_kernel_tool_grant(&request(None), Some(&json!({"results":[]})));
        let KernelToolGrantResolution::Granted(grant) = resolution else { panic!("expected grant") };
        assert!(grant.scope.is_none());
    }

    #[test]
    fn empty_allowlist_produces_empty_scope() {
        let scope = child_invoke_scope(&NestedHostScopeRequest { child_tool_names: Some(vec!["read".into()]), tool_allowlist: Some(vec![]), tool_denylist: Some(vec!["write".into()]), include_task_tools: false });
        assert_eq!(scope.tools.and_then(|v| v.allow), Some(vec![]));
    }

    #[test]
    fn descriptor_grant_carries_requested_name() {
        let resolution = resolve_kernel_tool_grant(&request(None), Some(&json!({"results":[{"name":"fixture_write","ok":true,"descriptor":{"name":"fixture_write","description":"","input_schema":{},"language":"js","kernel_generation":1,"definition_revision":1}}]})));
        let KernelToolGrantResolution::Granted(grant) = resolution else { panic!("expected grant") };
        assert_eq!(grant.requested_names, vec!["fixture_write"]);
    }

    #[test]
    fn refused_nested_host_tools_report_host_denied_code() {
        let result = kernel_tool_error_result("fixture_write", KernelToolErrorCode::KernelToolHostDenied, "write denied");
        assert_eq!(result["isError"], true);
        assert_eq!(result["details"]["error"]["code"], "kernel_tool_host_denied");
        assert_eq!(result["content"][0]["type"], "text");
    }

    #[test]
    fn scope_serializes_tools_policy_shape() {
        let scope = KernelToolInvokeScope { tools: Some(KernelToolHostScope { allow: Some(vec!["read".into()]), deny: Some(vec!["write".into()]) }) };
        assert_eq!(serde_json::to_value(scope).unwrap(), json!({"tools":{"allow":["read"],"deny":["write"]}}));
    }

    #[test]
    fn python_and_javascript_descriptors_are_supported() {
        for language in ["js", "py"] {
            assert!(is_kernel_tool_descriptor(&json!({"name":"tool","description":"d","input_schema":{},"language":language,"kernel_generation":1,"definition_revision":1})));
        }
    }
}

#[cfg(test)]
mod nested_host_scope_tests {
    use super::*;

    const CHILD_TOOLS: &[&str] = &["read", "grep", "ls", "web_search", "write", "edit", "bash", "lsp_symbols", "lsp_rename"];

    fn request() -> NestedHostScopeRequest {
        NestedHostScopeRequest { child_tool_names: Some(CHILD_TOOLS.iter().map(|v| (*v).into()).collect()), tool_allowlist: None, tool_denylist: None, include_task_tools: false }
    }

    #[test]
    fn read_and_interaction_tools_are_non_write() {
        for name in ["read", "grep", "ls", "web_search", "lsp_symbols", "ask_user_question"] { assert!(!is_write_capable_host_tool(name)); }
        for name in ["write", "edit", "bash", "memory", "lsp_rename", "mcp_unknown_tool"] { assert!(is_write_capable_host_tool(name)); }
    }

    #[test]
    fn effective_set_applies_deny_then_allow() {
        let mut denied = request();
        denied.tool_denylist = Some(vec!["write".into()]);
        assert!(!child_effective_tool_names(&denied).contains(&"write".into()));
        let mut allowed = request();
        allowed.tool_allowlist = Some(vec!["read".into()]);
        assert_eq!(child_effective_tool_names(&allowed), vec!["read"]);
        assert_eq!(child_effective_tool_names(&NestedHostScopeRequest { child_tool_names: Some(vec!["x_search".into()]), ..Default::default() }), vec!["read", "bash", "edit", "write", "grep", "x_search"]);
        assert_eq!(child_effective_tool_names(&NestedHostScopeRequest { child_tool_names: Some(vec![]), tool_allowlist: Some(vec!["read".into()]), ..Default::default() }), vec!["read"]);
    }

    #[test]
    fn no_policy_has_no_escalation() {
        assert_eq!(escalating_host_tools(&request()), Vec::<String>::new());
        let mut empty = request(); empty.tool_denylist = Some(vec![]);
        assert_eq!(escalating_host_tools(&empty), Vec::<String>::new());
    }

    #[test]
    fn empty_allowlist_escalates_every_write_tool() {
        let mut r = request(); r.tool_allowlist = Some(vec![]);
        assert_eq!(escalating_host_tools(&r), vec!["bash", "edit", "write", "lsp_rename"]);
        let r = NestedHostScopeRequest { child_tool_names: Some(vec!["fixture_write".into()]), tool_allowlist: Some(vec![]), tool_denylist: Some(vec!["write".into()]), include_task_tools: false };
        assert!(escalating_host_tools(&r).contains(&"write".into()));
    }

    #[test]
    fn denylist_only_escalates_removed_write_tools() {
        let mut r = request(); r.tool_denylist = Some(vec!["write".into(), "edit".into()]);
        assert_eq!(escalating_host_tools(&r), vec!["edit", "write"]);
        r.tool_denylist = Some(vec!["web_search".into(), "grep".into()]);
        assert_eq!(escalating_host_tools(&r), Vec::<String>::new());
    }

    #[test]
    fn allowlist_omissions_escalate_write_tools() {
        let mut r = request(); r.tool_allowlist = Some(vec!["read".into(), "grep".into(), "bash".into(), "write".into()]);
        assert_eq!(escalating_host_tools(&r), vec!["edit", "lsp_rename"]);
        r.tool_allowlist = Some(CHILD_TOOLS.iter().map(|v| (*v).into()).collect());
        assert_eq!(escalating_host_tools(&r), Vec::<String>::new());
    }

    #[test]
    fn excluded_tools_not_present_do_not_escalate() {
        let mut r = request(); r.tool_denylist = Some(vec!["memory".into(), "ask_user_question".into(), "task".into()]);
        assert_eq!(escalating_host_tools(&r), Vec::<String>::new());
        let r = NestedHostScopeRequest { child_tool_names: Some(vec!["read".into(), "task".into(), "workpool".into(), "team_send".into()]), tool_allowlist: Some(vec!["read".into(), "write".into(), "edit".into(), "bash".into()]), ..Default::default() };
        assert_eq!(escalating_host_tools(&r), Vec::<String>::new());
    }

    #[test]
    fn unknown_surface_still_checks_builtins() {
        let mut r = NestedHostScopeRequest { tool_denylist: Some(vec!["write".into()]), ..Default::default() };
        assert_eq!(escalating_host_tools(&r), vec!["write"]);
        r = NestedHostScopeRequest { child_tool_names: Some(vec![]), tool_allowlist: Some(vec!["read".into()]), ..Default::default() };
        assert_eq!(escalating_host_tools(&r), vec!["bash", "edit", "write"]);
        r.tool_denylist = Some(vec!["web_search".into()]);
        assert_eq!(escalating_host_tools(&r), Vec::<String>::new());
    }
}

#[cfg(test)]
mod kernel_tools_python_tests {
    use super::*;
    use crate::manager::execution_mode::ExecutionMode;
    use serde_json::json;

    fn request(names: &[&str]) -> KernelToolGrantRequest {
        KernelToolGrantRequest { requested_names: names.iter().map(|v| (*v).into()).collect(), capability: Some(KernelToolsCapability::default()), execution_mode: ExecutionMode::InProcess, agent_type: None, team_role_member: false, existing_tool_names: vec![], tool_allowlist: None, tool_denylist: None }
    }

    fn described(names: &[(&str, &str)]) -> serde_json::Value {
        json!({"results": names.iter().map(|(name, language)| json!({"name":name,"ok":true,"descriptor":{"name":name,"description":"","input_schema":{},"language":language,"kernel_generation":1,"definition_revision":1}})).collect::<Vec<_>>()})
    }

    #[test]
    fn python_grant_carries_descriptor() {
        let resolution = resolve_kernel_tool_grant(&request(&["add"]), Some(&described(&[("add", "py")])));
        let KernelToolGrantResolution::Granted(grant) = resolution else { panic!("expected grant") };
        assert_eq!(grant.descriptors[0].name, "add");
        assert_eq!(grant.descriptors[0].language, KernelToolLanguage::Py);
    }

    #[test]
    fn python_and_javascript_tools_are_granted_together() {
        let resolution = resolve_kernel_tool_grant(&request(&["lookup", "add"]), Some(&described(&[("lookup", "js"), ("add", "py")])));
        let KernelToolGrantResolution::Granted(grant) = resolution else { panic!("expected grant") };
        assert_eq!(grant.descriptors.iter().map(|v| v.language).collect::<Vec<_>>(), vec![KernelToolLanguage::Js, KernelToolLanguage::Py]);
    }

    #[test]
    fn only_js_and_python_descriptor_languages_validate() {
        for language in ["py", "rb", "jl"] {
            let descriptor = json!({"name":"add","description":"d","input_schema":{},"language":language,"kernel_generation":1,"definition_revision":1});
            assert_eq!(is_kernel_tool_descriptor(&descriptor), language == "py");
        }
    }

    #[test]
    fn undefined_name_is_coached_with_both_definitions() {
        let resolution = resolve_kernel_tool_grant(&request(&["missing"]), Some(&json!({"results":[{"name":"missing","ok":false,"error":{"code":"kernel_tool_missing","message":"Define with tool(function missing) or @tool def missing"}}]})));
        let KernelToolGrantResolution::Denied { code: KernelToolErrorCode::KernelToolMissing, message } = resolution else { panic!("expected missing denial") };
        assert!(message.contains("tool(function missing"));
        assert!(message.contains("@tool def missing"));
    }

    #[test]
    fn empty_request_produces_no_grant() {
        let resolution = resolve_kernel_tool_grant(&request(&[]), None);
        assert!(matches!(resolution, KernelToolGrantResolution::None));
    }
}

#[cfg(test)]
#[path = "kernel_tools_tests.rs"]
mod kernel_tools_port_tests;

#[cfg(test)]
#[path = "kernel_tools_invoke_scope_tests.rs"]
mod kernel_tools_invoke_scope_port_tests;

#[cfg(test)]
#[path = "kernel_tools_policy_refusal_tests.rs"]
mod kernel_tools_policy_refusal_port_tests;

#[cfg(test)]
#[path = "revive_tests.rs"]
mod revive_tests;
