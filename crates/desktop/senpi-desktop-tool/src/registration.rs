//! Registration exports matching upstream `src/registration.ts`.

pub use crate::command::{
    describe_capabilities, describe_stop_path, run_computer_command, status,
    COMPUTER_COMMAND_USAGE, COMPUTER_SUBCOMMANDS,
};
pub use crate::cua_definition::{
    computer_actions_params_schema, computer_actions_permission_parser,
    computer_actions_tool_definition, COMPUTER_ACTIONS_TOOL_NAME,
};
pub use crate::host_policy::{default_stop_hotkey, is_supported_host};
pub use crate::permission::{
    computer_permission_parser, computer_tier, PermissionRequest, COMPUTER_PERMISSION,
};
pub use crate::session::{
    run_snapshot, session_open_params, uses_coordinate_safe_image_sizing, ComputerHostContext,
    ComputerModel, ComputerSessionManager, SimpleSessionManager, AUDIT_FILE_NAME,
};
pub use crate::settings::{
    resolve_computer_settings, AuditLogSettings, ComputerSettings, ComputerSettingsError,
    ComputerSettingsInput, ScreenshotGcSettings,
};
pub use crate::skill::{
    computer_skill_markdown, materialize_computer_skill, COMPUTER_SKILL_DESCRIPTION,
    COMPUTER_SKILL_NAME,
};
pub use crate::tool_definition::{
    computer_tool_definition, kernel_prelude_for_computer, safety_guidelines, KernelPrelude,
    COMPUTER_DESCRIPTION, COMPUTER_TOOL_NAME, SEARCH_KEYWORDS,
};
