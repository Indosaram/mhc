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
pub use activation::{ActivationListener, ComputerDisabledError, ComputerHandle};
pub use command::{
    COMPUTER_COMMAND_USAGE, COMPUTER_SUBCOMMANDS, describe_capabilities, describe_stop_path,
    run_computer_command, status,
};
pub use cua_actions::{
    ACTION_NAMES, ComputerAction, ComputerActionsInput, ScreenshotBounds, actions_of,
    actions_script, is_read_only_actions, parse_computer_actions,
};
pub use cua_adapter::{ActionsOutcome, StepOutcome, create_computer_actions_tool};
pub use cua_definition::{
    COMPUTER_ACTIONS_TOOL_NAME, computer_actions_params_schema, computer_actions_permission_parser,
    computer_actions_tool_definition,
};
pub use cua_errors::{ComputerFailure, computer_failure};
pub use host_policy::{default_stop_hotkey, is_supported_host};
pub use params::{
    CallStep, ComputerToolParams, DEFAULT_TIMEOUT_SECONDS, MAX_TIMEOUT_SECONDS,
    computer_params_schema, parse_computer_params,
};
pub use permission::{
    COMPUTER_PERMISSION, PermissionRequest, computer_permission_parser, computer_tier,
};
pub use session::{
    AUDIT_FILE_NAME, ComputerHostContext, ComputerModel, ComputerSessionManager,
    SimpleSessionManager, run_snapshot, session_open_params, uses_coordinate_safe_image_sizing,
};
pub use settings::{
    AuditLogSettings, ComputerSettings, ComputerSettingsError, ComputerSettingsInput,
    ScreenshotGcSettings, resolve_computer_settings,
};
pub use skill::{
    COMPUTER_SKILL_DESCRIPTION, COMPUTER_SKILL_NAME, computer_skill_markdown,
    materialize_computer_skill,
};
pub use tool::{
    ComputerToolDeps, ComputerToolDetails, ComputerToolResult, create_computer_tool,
    render_call_chain, render_call_step, run_computer,
};
pub use tool_definition::{
    COMPUTER_DESCRIPTION, COMPUTER_TOOL_NAME, KernelPrelude, SEARCH_KEYWORDS,
    computer_tool_definition, kernel_prelude_for_computer, safety_guidelines,
};
