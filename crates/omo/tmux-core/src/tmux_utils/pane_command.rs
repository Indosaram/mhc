//! Shell command strings run inside tmux panes.

use utils::shell_escape_for_double_quoted_command;

use crate::env_source::{EnvSource, ProcessEnv, is_set};

const TMUX_COMMAND_SHELL: &str = "/bin/sh";

fn shell_quote_for_nested_command(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
        .replace('\\', "\\\\")
        .replace('$', "\\$")
        .replace('`', "\\`")
        .replace('"', "\\\"")
}

fn current_dir() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `/bin/sh -c "opencode attach <url> --session <id> --dir <dir>"` with every value
/// single-quoted for the nested shell. `None` or empty `directory` uses the cwd.
#[must_use]
pub fn build_tmux_attach_command(
    server_url: &str,
    session_id: &str,
    directory: Option<&str>,
) -> String {
    let directory = directory
        .filter(|directory| !directory.is_empty())
        .map_or_else(current_dir, str::to_owned);
    format!(
        "{TMUX_COMMAND_SHELL} -c \"opencode attach {} --session {} --dir {}\"",
        shell_quote_for_nested_command(server_url),
        shell_quote_for_nested_command(session_id),
        shell_quote_for_nested_command(&directory),
    )
}

/// An inert placeholder that prints a ready banner and sleeps until respawned.
#[must_use]
pub fn build_tmux_placeholder_command(description: &str) -> String {
    let escaped = shell_escape_for_double_quoted_command(description);
    format!(
        r#"{TMUX_COMMAND_SHELL} -c "printf '%s\n%s\n' \"OMO subagent pane ready: {escaped}\" \"Focus this pane to attach.\"; while :; do sleep 86400; done""#
    )
}

/// `-e OPENCODE_SERVER_PASSWORD=...` (plus username when set) for tmux env propagation.
pub fn build_pane_auth_environment_args_from(environment: &dyn EnvSource) -> Vec<String> {
    let password = environment.var("OPENCODE_SERVER_PASSWORD");
    if !is_set(password.as_deref()) {
        return Vec::new();
    }
    let mut args = vec![
        "-e".to_owned(),
        format!("OPENCODE_SERVER_PASSWORD={}", password.unwrap_or_default()),
    ];
    if let Some(username) = environment.var("OPENCODE_SERVER_USERNAME") {
        args.push("-e".to_owned());
        args.push(format!("OPENCODE_SERVER_USERNAME={username}"));
    }
    args
}

#[must_use]
pub fn build_pane_auth_environment_args() -> Vec<String> {
    build_pane_auth_environment_args_from(&ProcessEnv)
}
