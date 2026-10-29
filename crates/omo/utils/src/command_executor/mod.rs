//! Shell command execution: embedded `!`cmd`` resolution and hook commands with timeouts.

mod embedded_commands;
mod execute_command;
mod execute_hook_command;
mod home_directory;
mod resolve_commands_in_text;
mod shell_path;

pub use embedded_commands::{CommandMatch, find_embedded_commands};
pub use execute_command::execute_command;
pub use execute_hook_command::{CommandResult, ExecuteHookOptions, execute_hook_command};
pub use home_directory::get_home_directory;
pub use resolve_commands_in_text::{resolve_commands_in_text, resolve_commands_in_text_with_depth};
pub use shell_path::{find_bash_path, find_zsh_path};
