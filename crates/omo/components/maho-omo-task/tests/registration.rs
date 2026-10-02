pub mod support;
use maho_ext_api::FlagValue;
use maho_omo_task::registration::{register_task_flags,register_removed_team_wait_hint};
#[test] fn flags_default_enabled_and_preserve_explicit_host_override() {
    let mut api=support::api(); api.set_flag("omo-task",FlagValue::Boolean(false)); register_task_flags(&mut api);
    assert_eq!(api.get_flag("omo-task"),Some(FlagValue::Boolean(false))); assert_eq!(api.get_flag("omo-task-usage-hint"),Some(FlagValue::Boolean(true))); assert_eq!(api.registered.flags.len(),2);
    register_task_flags(&mut api); assert_eq!(api.registered.flags.len(),2); assert_eq!(api.get_flag("omo-task"),Some(FlagValue::Boolean(false)));
}
#[test] fn removed_team_wait_registers_hint_without_a_tool() { let mut api=support::api(); register_removed_team_wait_hint(&mut api); assert!(api.registered.removed_tool_hints.contains_key("team_wait")); assert!(api.registered.tools.is_empty()); }
