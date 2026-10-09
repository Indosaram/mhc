//! Native Rust port of upstream `@oh-my-opencode/senpi-desktop-tool` @ `selected455dee62`.
//!
//! Provides the `computer` tool, `computer_actions` adapter, `/computer` command,
//! settings resolution, permission classification, and skill materialization.

pub mod action_schema;
pub mod activation;
pub mod command;
pub mod cua_actions;
pub mod cua_adapter;
pub mod cua_definition;
pub mod cua_errors;
pub mod host_policy;
pub mod params;
pub mod permission;
pub mod registration;
pub mod session;
pub mod settings;
pub mod skill;
pub mod tool;
pub mod tool_definition;

pub use action_schema::ComputerArgumentsError;
pub use activation::{
    ActivationListener, ComputerDisabledError, ComputerHandle,
};
pub use command::{
    describe_capabilities, describe_stop_path, run_computer_command, status,
    COMPUTER_COMMAND_USAGE, COMPUTER_SUBCOMMANDS,
};
pub use cua_actions::{
    actions_of, actions_script, is_read_only_actions, parse_computer_actions, ComputerAction,
    ComputerActionsInput, ScreenshotBounds, ACTION_NAMES,
};
pub use cua_adapter::{
    create_computer_actions_tool, ActionsOutcome, StepOutcome,
};
pub use cua_definition::{
    computer_actions_params_schema, computer_actions_permission_parser,
    computer_actions_tool_definition, COMPUTER_ACTIONS_TOOL_NAME,
};
pub use cua_errors::{
    computer_failure, ComputerFailure,
};
pub use host_policy::{default_stop_hotkey, is_supported_host};
pub use params::{
    computer_params_schema, parse_computer_params, CallStep, ComputerToolParams,
    DEFAULT_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS,
};
pub use permission::{
    computer_permission_parser, computer_tier, PermissionRequest, COMPUTER_PERMISSION,
};
pub use session::{
    run_snapshot, session_open_params, uses_coordinate_safe_image_sizing, ComputerHostContext,
    ComputerModel, ComputerSessionManager, SimpleSessionManager, AUDIT_FILE_NAME,
};
pub use settings::{
    resolve_computer_settings, AuditLogSettings, ComputerSettings, ComputerSettingsError,
    ComputerSettingsInput, ScreenshotGcSettings,
};
pub use skill::{
    computer_skill_markdown, materialize_computer_skill, COMPUTER_SKILL_DESCRIPTION,
    COMPUTER_SKILL_NAME,
};
pub use tool::{
    create_computer_tool, render_call_chain, render_call_step, run_computer, ComputerToolDeps,
    ComputerToolDetails, ComputerToolResult,
};
pub use tool_definition::{
    computer_tool_definition, kernel_prelude_for_computer, safety_guidelines, KernelPrelude,
    COMPUTER_DESCRIPTION, COMPUTER_TOOL_NAME, SEARCH_KEYWORDS,
};
