//! `names.test.ts`

use crate::manager::names::NameRegistry;

#[test]
fn given_a_fresh_name_when_registered_then_it_is_used_verbatim_with_no_warning() {
    let mut registry = NameRegistry::default();
    let result = registry.register("parent-a", Some("reviewer"), None);
    assert_eq!(result.name, "reviewer");
    assert_eq!(result.warning, None);
}

#[test]
fn given_a_colliding_name_in_the_same_parent_when_registered_then_a_2_suffix_and_a_warning_are_returned()
 {
    let mut registry = NameRegistry::default();
    registry.register("parent-a", Some("reviewer"), None);
    let result = registry.register("parent-a", Some("reviewer"), None);
    assert_eq!(result.name, "reviewer-2");
    assert!(result.warning.expect("warning").contains("reviewer"));
}

#[test]
fn given_two_prior_collisions_when_registered_again_then_the_suffix_increments_to_3() {
    let mut registry = NameRegistry::default();
    registry.register("parent-a", Some("reviewer"), None);
    registry.register("parent-a", Some("reviewer"), None);
    let result = registry.register("parent-a", Some("reviewer"), None);
    assert_eq!(result.name, "reviewer-3");
}

#[test]
fn given_the_same_name_under_a_different_parent_when_registered_then_there_is_no_collision() {
    let mut registry = NameRegistry::default();
    registry.register("parent-a", Some("reviewer"), None);
    let result = registry.register("parent-b", Some("reviewer"), None);
    assert_eq!(result.name, "reviewer");
    assert_eq!(result.warning, None);
}

#[test]
fn given_no_requested_name_when_registered_then_an_auto_name_is_generated_and_reserved() {
    let mut registry = NameRegistry::default();
    let first = registry.register("parent-a", None, Some("st_00000001"));
    let second = registry.register("parent-a", None, Some("st_00000002"));
    assert_ne!(first.name, second.name);
}

#[test]
fn given_a_registered_name_when_released_and_re_registered_then_the_bare_name_is_available() {
    let mut registry = NameRegistry::default();
    registry.register("parent-a", Some("reviewer"), None);
    registry.release("parent-a", "reviewer");
    let result = registry.register("parent-a", Some("reviewer"), None);
    assert_eq!(result.name, "reviewer");
    assert_eq!(result.warning, None);
}

#[test]
fn given_an_unknown_name_when_released_then_registration_is_unchanged() {
    let mut registry = NameRegistry::default();
    registry.release("parent-a", "reviewer");
    let result = registry.register("parent-a", Some("reviewer"), None);
    assert_eq!(result.name, "reviewer");
    assert_eq!(result.warning, None);
}

#[test]
fn given_names_in_separate_parents_when_one_is_released_then_the_other_remains_reserved() {
    let mut registry = NameRegistry::default();
    registry.register("parent-a", Some("reviewer"), None);
    registry.register("parent-b", Some("reviewer"), None);
    registry.release("parent-a", "reviewer");
    let result = registry.register("parent-b", Some("reviewer"), None);
    assert_eq!(result.name, "reviewer-2");
    assert!(result.warning.expect("warning").contains("reviewer"));
}
