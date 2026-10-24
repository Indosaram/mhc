//! How the engine re-enters itself as a tool child. In senpi a compiled binary boots its embedded entry and an
//! interpreted install runs its own CLI entry under the same runtime; mhc is always the compiled executable, so the
//! child is this very binary (`std::env::current_exe()`) started with the internal-role environment variable. The
//! interpreted-install half (`toolChildEntryPath`, `runtimeExecArgv`, `resolveCliMainPathAsync`) has no Rust analogue.
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};

use crate::channel::TOOL_CHILD_ROLE_ENV;
use crate::entry::TOOL_CHILD_GENERATION_ENV;
use crate::launch_env_names::{
    ProcessEnv, TOOL_CHILD_LEDGER_ENV, TOOL_CHILD_REENTRY_ENV, TOOL_CHILD_REFS_ROOT_ENV,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolChildCommand {
    pub command: String,
    pub args: Vec<String>,
}

/// How this engine re-enters itself as a tool child: the running executable, with the role in its environment.
/// Never a PATH lookup.
pub fn tool_child_command() -> std::io::Result<ToolChildCommand> {
    let executable: PathBuf = std::env::current_exe()?;
    Ok(ToolChildCommand {
        command: executable.to_string_lossy().into_owned(),
        args: Vec::new(),
    })
}

pub struct SpawnToolChildOptions {
    pub generation: u32,
    pub channel_env: HashMap<String, String>,
    pub env: ProcessEnv,
    pub command: ToolChildCommand,
    pub cwd: PathBuf,
    /// Where both sides write payload references (`<root>/<generation>/`); the server owns and tears it down.
    pub refs_root: String,
    /// This generation's ledger: the child sweeps the owned groups it names if its host disappears.
    pub ledger_path: String,
}

pub fn spawn_tool_child(options: SpawnToolChildOptions) -> std::io::Result<Child> {
    let mut env = options.env;
    env.extend(options.channel_env);
    env.insert(TOOL_CHILD_ROLE_ENV.to_string(), "tool-child".to_string());
    env.insert(
        TOOL_CHILD_GENERATION_ENV.to_string(),
        options.generation.to_string(),
    );
    env.insert(TOOL_CHILD_REFS_ROOT_ENV.to_string(), options.refs_root);
    // How to re-enter this engine: the Windows launch shim is started the same way the child was.
    let reentry = serde_json::to_string(&options.command).map_err(std::io::Error::other)?;
    env.insert(TOOL_CHILD_REENTRY_ENV.to_string(), reentry);
    env.insert(TOOL_CHILD_LEDGER_ENV.to_string(), options.ledger_path);

    let mut command = Command::new(&options.command.command);
    command
        .args(&options.command.args)
        .current_dir(&options.cwd)
        .env_clear()
        .envs(&env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    // POSIX: the child leads its own group, so its whole tree can be signalled as one. Windows: the job is the handle.
    #[cfg(unix)]
    command.process_group(0);
    // `windowsHide: true`.
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_child_is_this_executable_with_no_arguments() {
        let command = tool_child_command().expect("current exe");
        assert_eq!(command.args, Vec::<String>::new());
        assert_eq!(
            std::path::Path::new(&command.command),
            std::env::current_exe().expect("current exe").as_path()
        );
    }

    #[test]
    fn the_reentry_command_round_trips_through_json() {
        let command = ToolChildCommand {
            command: "/bin/mhc".into(),
            args: vec!["a".into()],
        };
        let raw = serde_json::to_string(&command).expect("serialize");
        assert_eq!(raw, r#"{"command":"/bin/mhc","args":["a"]}"#);
        assert_eq!(
            serde_json::from_str::<ToolChildCommand>(&raw).expect("parse"),
            command
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_sets_the_role_generation_and_reentry_environment_and_leads_its_own_group() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = ToolChildCommand {
            command: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                format!(
                    "printf '%s|%s|%s|%s|%s|%s' \"${TOOL_CHILD_ROLE_ENV}\" \"${TOOL_CHILD_GENERATION_ENV}\" \
                     \"${TOOL_CHILD_REFS_ROOT_ENV}\" \"${TOOL_CHILD_LEDGER_ENV}\" \"$CHANNEL\" \"${TOOL_CHILD_REENTRY_ENV}\""
                ),
            ],
        };
        let reentry = serde_json::to_string(&command).expect("serialize");
        let child = spawn_tool_child(SpawnToolChildOptions {
            generation: 3,
            channel_env: HashMap::from([("CHANNEL".to_string(), "c".to_string())]),
            env: HashMap::new(),
            command,
            cwd: dir.path().to_path_buf(),
            refs_root: "/refs".into(),
            ledger_path: "/ledger".into(),
        })
        .expect("spawn");
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(30), child.wait_with_output())
                .await
                .expect("exit in time")
                .expect("output");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("tool-child|3|/refs|/ledger|c|{reentry}")
        );
    }
}
