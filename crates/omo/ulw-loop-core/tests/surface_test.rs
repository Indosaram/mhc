use ulw_loop_core::surface::*;
use std::collections::BTreeMap;

#[test]
fn no_marker_defaults_to_lazycodex() { assert_eq!(resolve_toolkit_surface(), UlwLoopToolkitSurface::LazyCodex); }

#[test]
fn omo_senpi_env_override_resolves_surface() {
    let env = BTreeMap::from([(String::from("OMO_AGENT_TOOLKIT_SURFACE"), String::from("omo-senpi"))]);
    assert_eq!(resolve_toolkit_surface_with_env(|key| env.get(key).cloned()), UlwLoopToolkitSurface::OmoSenpi);
}

#[test]
fn unknown_env_falls_back_to_lazycodex() {
    let env = BTreeMap::from([(String::from("OMO_AGENT_TOOLKIT_SURFACE"), String::from("bogus"))]);
    assert_eq!(resolve_toolkit_surface_with_env(|key| env.get(key).cloned()), UlwLoopToolkitSurface::LazyCodex);
}

#[test]
fn omo_senpi_surface_roles_match_namespace() { let roles=reviewer_roles_for(UlwLoopToolkitSurface::OmoSenpi); assert_eq!(roles.code_review,"omo-native-code-reviewer");assert_eq!(roles.manual_qa,"omo-native-qa-executor");assert_eq!(roles.gate_review,"omo-native-gate-reviewer"); }

#[test]
fn lazycodex_surface_roles_match_namespace() { let roles=reviewer_roles_for(UlwLoopToolkitSurface::LazyCodex); assert_eq!(roles.code_review,"lazycodex-code-reviewer");assert_eq!(roles.manual_qa,"lazycodex-qa-executor");assert_eq!(roles.gate_review,"lazycodex-gate-reviewer"); }

#[test]
fn reviewer_role_surface_set_contains_both_surfaces() { assert_eq!(all_reviewer_roles().len(),2); }

#[test]
fn canonicalizes_retired_reviewer_names() { assert_eq!(canonical_reviewer_agent_name("omo-senpi-gate-reviewer"),"omo-native-gate-reviewer");assert_eq!(canonical_reviewer_agent_name("omo-senpi-code-reviewer"),"omo-native-code-reviewer");assert_eq!(canonical_reviewer_agent_name("omo-senpi-qa-executor"),"omo-native-qa-executor"); }

#[test]
fn preserves_canonical_and_unrelated_names() { for name in ["omo-native-gate-reviewer","lazycodex-gate-reviewer","explore"] { assert_eq!(canonical_reviewer_agent_name(name),name); } }

#[test]
fn every_legacy_alias_targets_live_role() { let live=all_reviewer_roles().into_iter().flat_map(|r|[r.code_review,r.manual_qa,r.gate_review]).collect::<Vec<_>>(); for (_,canonical) in LEGACY_REVIEWER_AGENT_ALIASES { assert!(live.contains(&canonical)); } }

#[test]
fn gate_reviewer_names_include_canonical_and_retired() { assert!(GATE_REVIEWER_AGENT_NAMES.contains(&"omo-native-gate-reviewer"));assert!(GATE_REVIEWER_AGENT_NAMES.contains(&"omo-senpi-gate-reviewer"));assert!(GATE_REVIEWER_AGENT_NAMES.contains(&"lazycodex-gate-reviewer"));assert!(!GATE_REVIEWER_AGENT_NAMES.contains(&"omo-native-code-reviewer")); }

#[test]
fn existing_surface_default_contract() { assert_eq!(resolve_toolkit_surface(), UlwLoopToolkitSurface::LazyCodex); }

#[test]
fn existing_surface_reviewer_contract() { let roles=reviewer_roles_for(UlwLoopToolkitSurface::LazyCodex);assert_eq!(roles.code_review,"lazycodex-code-reviewer");let senpi=reviewer_roles_for(UlwLoopToolkitSurface::OmoSenpi);assert_eq!(senpi.gate_review,"omo-native-gate-reviewer"); }

#[test]
fn existing_surface_canonical_contract() { assert_eq!(canonical_reviewer_agent_name("other"),"other"); }

#[test]
fn test_resolve_toolkit_surface_default_preserved() {
    assert_eq!(resolve_toolkit_surface(), UlwLoopToolkitSurface::LazyCodex);
}

#[test]
fn test_reviewer_roles_for_surfaces_preserved() {
    let lazy = reviewer_roles_for(UlwLoopToolkitSurface::LazyCodex);
    assert_eq!(lazy.code_review, "lazycodex-code-reviewer");
    let senpi = reviewer_roles_for(UlwLoopToolkitSurface::OmoSenpi);
    assert_eq!(senpi.gate_review, "omo-native-gate-reviewer");
}

#[test]
fn test_canonical_reviewer_agent_name_preserved() {
    assert_eq!(canonical_reviewer_agent_name("omo-senpi-code-reviewer"), "omo-native-code-reviewer");
    assert_eq!(canonical_reviewer_agent_name("other"), "other");
}

#[test]
fn test_resolve_toolkit_surface_default() { assert_eq!(resolve_toolkit_surface(), UlwLoopToolkitSurface::LazyCodex); }

#[test]
fn test_reviewer_roles_for_surfaces() { let lazy=reviewer_roles_for(UlwLoopToolkitSurface::LazyCodex);assert_eq!(lazy.code_review,"lazycodex-code-reviewer");let senpi=reviewer_roles_for(UlwLoopToolkitSurface::OmoSenpi);assert_eq!(senpi.gate_review,"omo-native-gate-reviewer"); }

#[test]
fn test_canonical_reviewer_agent_name() { assert_eq!(canonical_reviewer_agent_name("omo-senpi-code-reviewer"),"omo-native-code-reviewer");assert_eq!(canonical_reviewer_agent_name("other"),"other"); }
