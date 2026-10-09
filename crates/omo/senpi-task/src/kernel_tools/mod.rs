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
