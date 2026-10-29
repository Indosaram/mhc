//! Port of `omo-senpi/src/extension/component-list.ts`: the ordered registration list.

use crate::support::{FakeComponent, manual_runtime_options, new_api};
use maho_ext_api::Extension;
use maho_omo::{
    OmoComponentOptions, OmoExtension, OmoSenpiComponent, RecordingLogger, omo_component_names,
    omo_components, try_omo_components,
};

fn options() -> OmoComponentOptions {
    let dir = std::env::temp_dir();
    OmoComponentOptions::default()
        .with_skills_root(dir.join("omo-skills"))
        .with_state_dir(dir.join("omo-state"))
}

fn slot(name: &'static str) -> OmoSenpiComponent {
    OmoSenpiComponent::new(
        name,
        Box::new(FakeComponent::new(move |api| {
            api.register_command(
                name,
                Some("slot".to_owned()),
                None,
                std::sync::Arc::new(|_, _| Box::pin(async { Ok(()) })),
            );
        })),
    )
}

#[test]
fn the_registration_order_is_the_full_upstream_list() {
    for facts_read_only_bootstrap in [false, true] {
        let mut options = options();
        options.facts_read_only_bootstrap = facts_read_only_bootstrap;
        let components = omo_components(slot("task"), slot("memory"), &options);
        let names: Vec<&str> = components.iter().map(|component| component.name).collect();
        assert_eq!(names, omo_component_names());
        assert_eq!(names.len(), 30);
        assert!(!names.contains(&"native-badge"));
        assert_eq!(names[6], "onboarding-scan");
        assert_eq!(names[7], "side-panel");
        assert_eq!(names[14], "question-gates");
        assert_eq!(names[15], "claim-receipt");
        assert_eq!(names[21], "x-search");
        assert_eq!(names[22], "feedback");
        assert_eq!(names[23], "browser-bridge");
        assert_eq!(names[24], "desktop-toolkit");
        assert_eq!(names[25], "lsp");
        assert_eq!(names[26], "task");
        assert_eq!(names[27], "memory");
        assert_eq!(names[28], "gateway");
        assert_eq!(names[29], "config-watch");
    }
}

#[test]
fn a_missing_slot_is_an_explicit_error_not_a_short_list() {
    assert_eq!(
        try_omo_components(None, None, &options())
            .expect_err("both missing")
            .names,
        vec!["task", "memory"]
    );
    assert_eq!(
        try_omo_components(Some(slot("task")), None, &options())
            .expect_err("memory missing")
            .names,
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
    assert_eq!(names.len(), 30);
}

/// `documented-commands.test.ts` case 3: bare skills must be rewritten before other input handlers.
#[test]
fn skill_commands_precede_every_input_handler_component() {
    let components = omo_components(slot("task"), slot("memory"), &options());
    let names: Vec<&str> = components.iter().map(|component| component.name).collect();
    let skill_commands = names
        .iter()
        .position(|name| *name == "skill-commands")
        .expect("skill-commands is registered");
    for handler in [
        "telemetry",
        "ultrawork",
        "skill-pointers",
        "ulw-execute-continuation",
        "ulw-loop",
        "fallback-architect",
        "task",
        "memory",
    ] {
        let index = names
            .iter()
            .position(|name| *name == handler)
            .unwrap_or_else(|| panic!("{handler} is registered"));
        assert!(
            skill_commands < index,
            "skill-commands must precede the {handler} input handler"
        );
    }
}

pub(crate) fn the_registered_loop_component_never_reports_itself_inactive() {
    // Pinned omo-senpi ulw-loop runs in-process: with no toolkit CLI on the host the registered
    // component still runs and never reports itself inactive through the shared runtime logger.
    let loop_options = OmoComponentOptions {
        skills_root: std::env::temp_dir().join("omo-skills"),
        state_dir: std::env::temp_dir().join("omo-state"),
        env: std::collections::BTreeMap::new(),
        facts_read_only_bootstrap: false,
    };
    let loop_component = omo_components(slot("task"), slot("memory"), &loop_options)
        .into_iter()
        .find(|component| component.name == "ulw-loop")
        .expect("the ulw-loop component is in the list");
    let recording = std::sync::Arc::new(RecordingLogger::new());
    let extension = OmoExtension::with_options(
        vec![loop_component],
        manual_runtime_options(recording.clone()),
        Default::default(),
    );

    extension.register(&mut new_api());

    assert!(
        !recording
            .entries()
            .iter()
            .any(|entry| entry.message.contains("ulw-loop inactive")),
        "the in-process loop never reports itself inactive: {:?}",
        recording.entries()
    );
}
