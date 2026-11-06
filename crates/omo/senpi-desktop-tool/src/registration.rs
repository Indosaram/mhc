//! Registration exports matching upstream `src/registration.ts`.

pub use crate::command::{
    COMPUTER_COMMAND_USAGE, COMPUTER_SUBCOMMANDS, describe_capabilities, describe_stop_path,
    run_computer_command, status,
};
pub use crate::cua_definition::{
    COMPUTER_ACTIONS_TOOL_NAME, computer_actions_params_schema, computer_actions_permission_parser,
    computer_actions_tool_definition,
};
pub use crate::host_policy::{default_stop_hotkey, is_supported_host};
pub use crate::permission::{
    COMPUTER_PERMISSION, PermissionRequest, computer_permission_parser, computer_tier,
};
pub use crate::session::{
    AUDIT_FILE_NAME, ComputerHostContext, ComputerModel, ComputerSessionManager,
    SimpleSessionManager, run_snapshot, session_open_params, uses_coordinate_safe_image_sizing,
};
pub use crate::settings::{
    AuditLogSettings, ComputerSettings, ComputerSettingsError, ComputerSettingsInput,
    ScreenshotGcSettings, resolve_computer_settings,
};
pub use crate::skill::{
    COMPUTER_SKILL_DESCRIPTION, COMPUTER_SKILL_NAME, computer_skill_markdown,
    materialize_computer_skill,
};
pub use crate::tool_definition::{
    COMPUTER_DESCRIPTION, COMPUTER_TOOL_NAME, KernelPrelude, SEARCH_KEYWORDS,
    computer_tool_definition, kernel_prelude_for_computer, safety_guidelines,
};
