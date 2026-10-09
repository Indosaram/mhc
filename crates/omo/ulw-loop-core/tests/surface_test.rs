use ulw_loop_core::surface::*;

#[test]
fn test_resolve_toolkit_surface_default() {
    assert_eq!(resolve_toolkit_surface(), UlwLoopToolkitSurface::LazyCodex);
}

#[test]
fn test_reviewer_roles_for_surfaces() {
    let lazy = reviewer_roles_for(UlwLoopToolkitSurface::LazyCodex);
    assert_eq!(lazy.code_review, "lazycodex-code-reviewer");
    assert_eq!(lazy.manual_qa, "lazycodex-qa-executor");
    assert_eq!(lazy.gate_review, "lazycodex-gate-reviewer");

    let senpi = reviewer_roles_for(UlwLoopToolkitSurface::OmoSenpi);
    assert_eq!(senpi.code_review, "omo-native-code-reviewer");
    assert_eq!(senpi.manual_qa, "omo-native-qa-executor");
    assert_eq!(senpi.gate_review, "omo-native-gate-reviewer");
}

#[test]
fn test_canonical_reviewer_agent_name() {
    assert_eq!(canonical_reviewer_agent_name("omo-senpi-code-reviewer"), "omo-native-code-reviewer");
    assert_eq!(canonical_reviewer_agent_name("omo-senpi-qa-executor"), "omo-native-qa-executor");
    assert_eq!(canonical_reviewer_agent_name("omo-senpi-gate-reviewer"), "omo-native-gate-reviewer");
    assert_eq!(canonical_reviewer_agent_name("other"), "other");
}
