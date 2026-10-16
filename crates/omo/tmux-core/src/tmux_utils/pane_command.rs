//! Shell command strings run inside tmux panes.

use crate::env_source::{EnvSource, ProcessEnv, is_set};

const TMUX_COMMAND_SHELL: &str = "/bin/sh";
const PLACEHOLDER_DESCRIPTION_LIMIT: usize = 2_000;
const PANE_TITLE_DESCRIPTION_LIMIT: usize = 20;

fn shell_quote_for_nested_command(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
        .replace('\\', "\\\\")
        .replace('$', "\\$")
        .replace('`', "\\`")
        .replace('"', "\\\"")
}

fn double_quote_shell_argument(value: &str) -> String {
    value
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

#[must_use]
pub fn escape_tmux_format(value: &str) -> String {
    value.replace('#', "##")
}

#[must_use]
pub fn build_subagent_pane_title(description: &str) -> String {
    let clean: String = description.chars().filter(|character| *character != '\0').collect();
    let shown: String = clean.chars().take(PANE_TITLE_DESCRIPTION_LIMIT).collect();
    format!("omo-subagent-{}", escape_tmux_format(&shown))
}

/// An inert placeholder that prints a ready banner and sleeps until respawned.
#[must_use]
pub fn build_tmux_placeholder_command(description: &str) -> String {
    let clean: String = description.chars().filter(|character| *character != '\0').collect();
    let shown: String = if clean.chars().count() > PLACEHOLDER_DESCRIPTION_LIMIT {
        format!("{}…", clean.chars().take(PLACEHOLDER_DESCRIPTION_LIMIT).collect::<String>())
    } else {
        clean
    };
    let banner = double_quote_shell_argument(&format!("OMO subagent pane ready: {shown}"));
    let hint = double_quote_shell_argument("Focus this pane to attach.");
    format!(
        r#"{TMUX_COMMAND_SHELL} -c "printf '%s\n%s\n' \"{banner}\" \"{hint}\"; while :; do sleep 86400; done""#
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
