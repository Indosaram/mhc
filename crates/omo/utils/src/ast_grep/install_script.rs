use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use super::runtime_slug;
use crate::runtime::node_platform;

pub const AST_GREP_BIN_DIR_ENV_KEY: &str = "OMO_AST_GREP_BIN_DIR";
pub const KILL_GRACE_MS: u64 = 1_000;
const AST_GREP_INSTALL_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AstGrepInstallSpawnOutcome {
    Exit {
        code: Option<i32>,
        signal: Option<i32>,
    },
    SpawnError {
        message: String,
        missing_executable: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AstGrepInstallSpawnOptions {
    pub cwd: String,
    pub env: HashMap<String, String>,
}

/// A spawned install child: `outcome` delivers exactly one result, `kill` requests termination.
pub struct AstGrepInstallSpawnedProcess {
    pub kill: Box<dyn FnMut() + Send>,
    pub outcome: Receiver<AstGrepInstallSpawnOutcome>,
}

pub type AstGrepInstallSpawn<'a> =
    &'a dyn Fn(&str, &[String], &AstGrepInstallSpawnOptions) -> AstGrepInstallSpawnedProcess;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AstGrepSkillInstallResult {
    Succeeded,
    Skipped { reason: String },
    Failed { reason: String },
    TimedOut,
}

pub struct AstGrepSkillInstallOptions<'a> {
    /// Base environment; `None` inherits the process environment.
    pub env: Option<&'a HashMap<String, String>>,
    pub file_exists: Option<&'a dyn Fn(&str) -> bool>,
    pub platform: Option<&'a str>,
    pub skill_dir: String,
    pub spawn_process: Option<AstGrepInstallSpawn<'a>>,
    pub target_dir: String,
    pub timeout_ms: Option<u64>,
}

pub fn ast_grep_runtime_dir(base_dir: &str, platform: &str, arch: &str) -> String {
    Path::new(base_dir)
        .join("runtime")
        .join("ast-grep")
        .join(runtime_slug(platform, arch))
        .to_string_lossy()
        .into_owned()
}

fn default_spawn(
    command: &str,
    args: &[String],
    options: &AstGrepInstallSpawnOptions,
) -> AstGrepInstallSpawnedProcess {
    let (tx, rx) = mpsc::channel();
    let spawned = Command::new(command)
        .args(args)
        .current_dir(&options.cwd)
        .env_clear()
        .envs(&options.env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let child = match spawned {
        Ok(child) => Arc::new(Mutex::new(child)),
        Err(error) => {
            let _ = tx.send(AstGrepInstallSpawnOutcome::SpawnError {
                missing_executable: error.kind() == std::io::ErrorKind::NotFound,
                message: error.to_string(),
            });
            return AstGrepInstallSpawnedProcess {
                kill: Box::new(|| {}),
                outcome: rx,
            };
        }
    };
    let waiter = Arc::clone(&child);
    thread::spawn(move || {
        loop {
            let status = waiter
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .try_wait();
            match status {
                Ok(Some(status)) => {
                    #[cfg(unix)]
                    let signal = std::os::unix::process::ExitStatusExt::signal(&status);
                    #[cfg(not(unix))]
                    let signal = None;
                    let _ = tx.send(AstGrepInstallSpawnOutcome::Exit {
                        code: status.code(),
                        signal,
                    });
                    return;
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(error) => {
                    let _ = tx.send(AstGrepInstallSpawnOutcome::SpawnError {
                        message: error.to_string(),
                        missing_executable: false,
                    });
                    return;
                }
            }
        }
    });
    AstGrepInstallSpawnedProcess {
        kill: Box::new(move || {
            let _ = child.lock().unwrap_or_else(PoisonError::into_inner).kill();
        }),
        outcome: rx,
    }
}

enum Timed {
    Outcome(AstGrepInstallSpawnOutcome),
    TimedOut,
}

fn run_invocation(spawned: AstGrepInstallSpawnedProcess, timeout_ms: u64) -> Timed {
    let AstGrepInstallSpawnedProcess { mut kill, outcome } = spawned;
    match outcome.recv_timeout(Duration::from_millis(timeout_ms)) {
        Ok(result) => return Timed::Outcome(result),
        Err(RecvTimeoutError::Disconnected) => {
            return Timed::Outcome(AstGrepInstallSpawnOutcome::SpawnError {
                message: "ast-grep install child vanished without an outcome".to_string(),
                missing_executable: false,
            });
        }
        Err(RecvTimeoutError::Timeout) => {}
    }
    kill();
    match outcome.recv_timeout(Duration::from_millis(KILL_GRACE_MS)) {
        Ok(_) => Timed::TimedOut,
        Err(_) => Timed::Outcome(AstGrepInstallSpawnOutcome::SpawnError {
            message: "ast-grep install child ignored termination".to_string(),
            missing_executable: false,
        }),
    }
}

/// Run the skill's `install.sh` (or `install.ps1` via pwsh, then powershell.exe) with
/// `OMO_AST_GREP_BIN_DIR` pointed at `target_dir`, bounded by a timeout.
pub fn run_ast_grep_skill_install(
    options: &AstGrepSkillInstallOptions<'_>,
) -> AstGrepSkillInstallResult {
    let platform = options.platform.unwrap_or(node_platform());
    let windows = platform == "win32";
    let script = Path::new(&options.skill_dir)
        .join(if windows { "install.ps1" } else { "install.sh" })
        .to_string_lossy()
        .into_owned();
    let exists = options.file_exists.map_or_else(
        || Path::new(&script).exists(),
        |file_exists| file_exists(&script),
    );
    if !exists {
        return AstGrepSkillInstallResult::Skipped {
            reason: format!("missing {script}"),
        };
    }
    let mut env: HashMap<String, String> = options
        .env
        .cloned()
        .unwrap_or_else(|| std::env::vars().collect());
    env.insert(
        AST_GREP_BIN_DIR_ENV_KEY.to_string(),
        options.target_dir.clone(),
    );
    let spawn_options = AstGrepInstallSpawnOptions {
        cwd: options.skill_dir.clone(),
        env,
    };
    let invocations: Vec<(&str, Vec<String>)> = if windows {
        let args: Vec<String> = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", &script]
            .map(str::to_string)
            .to_vec();
        vec![("pwsh", args.clone()), ("powershell.exe", args)]
    } else {
        vec![("bash", vec![script.clone()])]
    };
    let spawn = options.spawn_process.unwrap_or(&default_spawn);
    let timeout_ms = options.timeout_ms.unwrap_or(AST_GREP_INSTALL_TIMEOUT_MS);
    for (command, args) in &invocations {
        match run_invocation(spawn(command, args, &spawn_options), timeout_ms) {
            Timed::TimedOut => return AstGrepSkillInstallResult::TimedOut,
            Timed::Outcome(AstGrepInstallSpawnOutcome::Exit { code: Some(0), .. }) => {
                return AstGrepSkillInstallResult::Succeeded;
            }
            Timed::Outcome(AstGrepInstallSpawnOutcome::Exit { code, signal }) => {
                let status = code
                    .map(|c| c.to_string())
                    .or_else(|| signal.map(|s| format!("signal {s}")))
                    .unwrap_or_else(|| "without status".to_string());
                return AstGrepSkillInstallResult::Failed {
                    reason: format!("{command} exited {status}"),
                };
            }
            Timed::Outcome(AstGrepInstallSpawnOutcome::SpawnError {
                missing_executable: true,
                ..
            }) if windows && *command == "pwsh" => {}
            Timed::Outcome(AstGrepInstallSpawnOutcome::SpawnError { message, .. }) => {
                return AstGrepSkillInstallResult::Failed { reason: message };
            }
        }
    }
    AstGrepSkillInstallResult::Failed {
        reason: "no ast-grep install shell was available".to_string(),
    }
}
