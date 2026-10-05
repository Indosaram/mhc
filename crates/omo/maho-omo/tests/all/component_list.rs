//! Port of `omo-senpi/src/extension/component-list.ts`: the ordered registration list.

use maho_ext_api::Extension;
use maho_omo::{OmoComponentOptions, OmoExtension, OmoSenpiComponent, RecordingLogger, omo_component_names, omo_components, try_omo_components};
use crate::support::{FakeComponent, manual_runtime_options, new_api};

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

#[test]
fn the_registered_loop_component_logs_through_the_shared_runtime_logger() {
    // An env with no omo binary makes the loop's registration path take the inactive branch,
    // which must log through the shared runtime logger (not only in tests that inject one).
    let loop_options = OmoComponentOptions {
        skills_root: std::env::temp_dir().join("omo-skills"),
        state_dir: std::env::temp_dir().join("omo-state"),
        env: std::collections::BTreeMap::new(),
    };
    let loop_component = omo_components(slot("task"), slot("memory"), &loop_options)
        .into_iter()
        .find(|component| component.name == "ulw-loop")
        .expect("the ulw-loop component is in the list");
    let recording = std::sync::Arc::new(RecordingLogger::new());
    let extension = OmoExtension::with_options(vec![loop_component], manual_runtime_options(recording.clone()), Default::default());

    extension.register(&mut new_api());

    assert!(
        recording.entries().iter().any(|entry| entry.message.contains("ulw-loop inactive")),
        "loop registration logs the inactive message through the shared logger: {:?}",
        recording.entries()
    );
}
