use maho_ext_api::{ExtensionApi,FlagType};
pub fn register_task_flags(api:&mut ExtensionApi) {
    api.register_flag("omo-task",FlagType::Boolean { default:Some(true) },Some("Enable the omo-senpi task engine (use --no-omo-task to disable).".into()));
    api.register_flag("omo-task-usage-hint",FlagType::Boolean { default:Some(true) },Some("Inject once-per-session omo-senpi task usage guidance.".into()));
}
pub fn register_removed_team_wait_hint(api:&mut ExtensionApi) {
    api.register_removed_tool_hint("team_wait","team_wait was removed - team messages arrive as steered notifications; send updates with task_send and end your turn.");
}
