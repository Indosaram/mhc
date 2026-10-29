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
    let banner = shell_quote_for_nested_command(&format!("OMO subagent pane ready: {shown}"));
    let hint = shell_quote_for_nested_command("Focus this pane to attach.");
    format!(
        r#"{TMUX_COMMAND_SHELL} -c "printf '%s\n%s\n' {banner} {hint}; while :; do sleep 86400; done""#
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn run_placeholder(description: &str) -> String {
        let temp = tempfile::tempdir().expect("temporary placeholder directory");
        let sleep = temp.path().join("sleep");
        std::fs::write(&sleep, "#!/bin/sh\nkill -TERM \"$PPID\"\n")
            .expect("write fake sleep that terminates the idle shell");
        std::fs::set_permissions(&sleep, std::fs::Permissions::from_mode(0o755))
            .expect("make fake sleep executable");
        let command = build_tmux_placeholder_command(description);
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &command])
            .env("PATH", format!("{}:{}", temp.path().display(), std::env::var("PATH").unwrap_or_default()))
            .env("OMO_TMUX_CANARY_VAR", "LEAKED")
            .output()
            .expect("run nested placeholder shell");
        String::from_utf8(output.stdout).expect("UTF-8 placeholder banner")
    }

    #[test]
    fn placeholder_prints_shell_syntax_and_edge_cases_literally() {
        let temp = tempfile::tempdir().expect("temporary canary directory");
        let canary = temp.path().join("canary");
        for description in [
            "Fix Bob's 100% broken pane".to_owned(),
            format!("run $(touch {}) `touch {}` $HOME \"quoted\" a;b|c&d #e (f) back\\slash", canary.display(), canary.display()),
            format!("first line\nsecond line $(touch {})", canary.display()),
            "ends in a backslash \\".to_owned(),
            "expands ${OMO_TMUX_CANARY_VAR} and $OMO_TMUX_CANARY_VAR".to_owned(),
            "before\0after".to_owned(),
        ] {
            let output = run_placeholder(&description);
            assert_eq!(output, format!("OMO subagent pane ready: {}\nFocus this pane to attach.\n", description.replace('\0', "")));
            assert!(!canary.exists());
        }
    }

    #[test]
    fn placeholder_caps_long_descriptions_before_shell_execution() {
        let description = "$(printf UNEXPECTED) ".repeat(2_000);
        let command = build_tmux_placeholder_command(&description);
        assert!(command.len() < 12_000);
        assert_eq!(run_placeholder(&description), format!(
            "OMO subagent pane ready: {}…\nFocus this pane to attach.\n",
            &description[..2_000],
        ));
    }
}
