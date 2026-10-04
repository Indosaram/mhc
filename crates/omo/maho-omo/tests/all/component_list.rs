//! Port of `omo-senpi/src/extension/component-list.ts`: the ordered registration list.

use maho_omo::{OmoComponentOptions, OmoSenpiComponent, omo_component_names, omo_components, try_omo_components};
use crate::support::FakeComponent;

fn options() -> OmoComponentOptions {
    let dir = std::env::temp_dir();
    OmoComponentOptions::default()
        .with_skills_root(dir.join("omo-skills"))
        .with_state_dir(dir.join("omo-state"))
}

fn slot(name: &'static str) -> OmoSenpiComponent {
    OmoSenpiComponent::new(name, Box::new(FakeComponent::new(move |api| {
        api.register_command(name, Some("slot".to_owned()), None, std::sync::Arc::new(|_, _| Box::pin(async { Ok(()) })));
    })))
}

#[test]
fn the_registration_order_is_the_full_upstream_list() {
    let components = omo_components(slot("task"), slot("memory"), &options());

    let names: Vec<&str> = components.iter().map(|component| component.name).collect();
    assert_eq!(names, omo_component_names());
    assert_eq!(names.len(), 17);
    assert_eq!(names[14], "task");
    assert_eq!(names[15], "memory");
    assert_eq!(names[16], "config-watch");
}

#[test]
fn a_missing_slot_is_an_explicit_error_not_a_short_list() {
    assert_eq!(
        try_omo_components(None, None, &options()).expect_err("both missing").names,
        vec!["task", "memory"]
    );
    assert_eq!(
        try_omo_components(Some(slot("task")), None, &options()).expect_err("memory missing").names,
        vec!["memory"]
    );
    assert!(try_omo_components(Some(slot("task")), Some(slot("memory")), &options()).is_ok());
}

#[test]
fn every_registered_component_has_a_distinct_name() {
    let components = omo_components(slot("task"), slot("memory"), &options());

    let mut names: Vec<&str> = components.iter().map(|component| component.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 17);
}
