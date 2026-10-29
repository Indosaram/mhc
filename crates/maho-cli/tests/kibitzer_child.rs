use maho_cli::cli::kibitzer_child::{KIBITZER_SIDECAR_TOOL_NAMES, KibitzerChildResources};

#[test]
fn the_sidecar_registry_order_is_the_pinned_five() {
    assert_eq!(
        KIBITZER_SIDECAR_TOOL_NAMES,
        ["read", "grep", "session_entries", "memory", "nudge"]
    );
}

#[test]
fn an_empty_resource_set_is_a_child_with_no_tools_not_a_builtin_passthrough() {
    let resources = KibitzerChildResources::default();
    assert!(resources.tools.is_empty());
    assert!(resources.persona.is_empty());
}
