//! Native extension-entry tests: the `maho-omo` flag set and the disabled short-circuit, the
//! native half of `omo-senpi/src/extension/index.test.ts`.

use maho_ext_api::Extension;
use maho_omo::{OMO_DISABLED_FLAG, OmoSenpiComponent, OmoRuntimeOptions, RecordingLogger, component_disabled_flag, compose_omo_extension_with_options};
use crate::support::{FakeComponent, flag_names, manual_runtime_options, new_api, set_boolean_flag, tool_names};

fn options(logger: &RecordingLogger) -> OmoRuntimeOptions {
    manual_runtime_options(std::sync::Arc::new(logger.clone()))
}

fn component(name: &'static str, tool: &'static str) -> OmoSenpiComponent {
    OmoSenpiComponent::new(name, Box::new(FakeComponent::new(move |api| api.register_tool(crate::support::fake_tool(tool)))))
}

#[test]
fn the_entry_registers_the_disabled_flag_and_one_flag_per_component() {
    let logger = RecordingLogger::new();
    let extension = compose_omo_extension_with_options(
        vec![
            component("config-startup", "config_startup_tool"),
            component("config-watch", "config_watch_tool"),
            component("ulw-loop", "ulw_loop_tool"),
        ],
        options(&logger),
        Default::default(),
    );

    let mut api = new_api();
    extension.register(&mut api);

    assert_eq!(
        flag_names(&api),
        vec![
            OMO_DISABLED_FLAG.to_owned(),
            component_disabled_flag("config-startup"),
            component_disabled_flag("config-watch"),
            component_disabled_flag("ulw-loop"),
        ]
    );
    assert_eq!(
        tool_names(&api),
        vec!["config_startup_tool".to_owned(), "config_watch_tool".to_owned(), "ulw_loop_tool".to_owned()]
    );
}

#[test]
fn a_disabled_component_emits_no_registration_and_one_info_log() {
    let logger = RecordingLogger::new();
    let extension = compose_omo_extension_with_options(
        vec![component("config-watch", "config_watch_tool")],
        options(&logger),
        Default::default(),
    );

    let mut api = new_api();
    set_boolean_flag(&mut api, &component_disabled_flag("config-watch"), true);
    extension.register(&mut api);

    assert!(tool_names(&api).is_empty());
    assert_eq!(logger.entries().len(), 1);
    assert_eq!(logger.entries()[0].message, "omo-senpi component disabled by flag");
}
