use crate::lsp::cleanup_errors::report_best_effort_cleanup_error;
use crate::lsp::errors::LspError;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Child;
use tokio::process::ChildStderr;
use tokio::process::ChildStdin;
use tokio::process::ChildStdout;
use tokio::process::Command;

/// TS `validateCwd` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CwdValidation {
    pub valid: bool,
    pub error: Option<String>,
}

/// TS `PreparedSpawnCommand` (`shell` is always false).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSpawnCommand {
    pub command: String,
    pub args: Vec<String>,
    pub shell: bool,
}

/// TS `SpawnOptions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnOptions {
    pub cwd: String,
    pub env: BTreeMap<String, String>,
}

/// TS `SpawnedProcess`: the child plus its three pipes (taken once by the transport).
#[derive(Debug)]
pub struct SpawnedProcess {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    pub pid: Option<u32>,
}

impl SpawnedProcess {
    /// TS `exitCode`: `None` while running. A signal-terminated child reports `Some(1)`.
    pub fn exit_code(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(1)),
            Ok(None) => None,
            Err(_) => Some(1),
        }
    }

    /// TS `kill(signal)`: kills the whole process group on Unix.
    pub fn kill(&mut self, signal: KillSignal) {
        kill_process_tree(&mut self.child, self.pid, signal);
    }

    /// TS `exited`: resolves with the exit code (0 when unknown, 1 on error).
    pub async fn exited(&mut self) -> i32 {
        match self.child.wait().await {
            Ok(status) => status.code().unwrap_or(0),
            Err(_) => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSignal {
    Term,
    Kill,
}

/// TS `validateCwd`.
pub fn validate_cwd(cwd: &str) -> CwdValidation {
    let invalid = |error: String| CwdValidation {
        valid: false,
        error: Some(error),
    };
    let path = Path::new(cwd);
    if !path.exists() {
        return invalid(format!("Working directory does not exist: {cwd}"));
    }
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => CwdValidation {
            valid: true,
            error: None,
        },
        Ok(_) => invalid(format!("Path is not a directory: {cwd}")),
        Err(error) => invalid(format!("Cannot access working directory: {cwd} ({error})")),
    }
}

/// Target platform for [`create_spawn_command`] (TS `NodeJS.Platform` subset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnPlatform {
    Windows,
    Unix,
}

impl SpawnPlatform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// TS `createSpawnCommand`.
pub fn create_spawn_command(
    command: &[String],
    platform: SpawnPlatform,
    command_processor: &str,
    env: &BTreeMap<String, String>,
) -> Result<PreparedSpawnCommand, LspError> {
    let Some((cmd, args)) = command.split_first() else {
        return Err(LspError::ProcessSpawn("[lsp] empty command".to_string()));
    };
    if platform == SpawnPlatform::Unix {
        return Ok(PreparedSpawnCommand {
            command: cmd.clone(),
            args: args.to_vec(),
            shell: false,
        });
    }
    let resolved = resolve_windows_command(cmd, env);
    let lower = resolved.to_lowercase();
    if !(lower.ends_with(".cmd") || lower.ends_with(".bat")) {
        return Ok(PreparedSpawnCommand {
            command: resolved,
            args: args.to_vec(),
            shell: false,
        });
    }
    let mut wrapped = vec![
        "/d".to_string(),
        "/s".to_string(),
        "/c".to_string(),
        resolved,
    ];
    wrapped.extend(args.iter().cloned());
    Ok(PreparedSpawnCommand {
        command: command_processor.to_string(),
        args: wrapped,
        shell: false,
    })
}

fn resolve_windows_command(command: &str, env: &BTreeMap<String, String>) -> String {
    let has_separator = command.contains('/') || command.contains('\\');
    let path_value = env
        .get("PATH")
        .or_else(|| env.get("Path"))
        .cloned()
        .unwrap_or_default();
    let bases: Vec<String> = if has_separator {
        vec![String::new()]
    } else {
        path_value
            .split(';')
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect()
    };
    let raw = env
        .get("PATHEXT")
        .cloned()
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
    let mut extensions: Vec<String> = Vec::new();
    let configured = raw
        .split(';')
        .map(str::trim)
        .filter(|ext| !ext.is_empty())
        .map(|ext| {
            if ext.starts_with('.') {
                ext.to_string()
            } else {
                format!(".{ext}")
            }
        });
    for extension in configured.chain([".exe", ".cmd", ".bat", ""].map(str::to_string)) {
        if !extensions.contains(&extension) {
            extensions.push(extension);
        }
    }
    for base in &bases {
        for extension in &extensions {
            let candidate = if base.is_empty() {
                format!("{command}{extension}")
            } else {
                Path::new(base)
                    .join(format!("{command}{extension}"))
                    .to_string_lossy()
                    .into_owned()
            };
            if Path::new(&candidate).exists() {
                return candidate;
            }
        }
    }
    command.to_string()
}

/// TS `spawnProcess`: piped stdio, detached into its own process group on Unix.
pub fn spawn_process(
    command: &[String],
    options: &SpawnOptions,
) -> Result<SpawnedProcess, LspError> {
    let validation = validate_cwd(&options.cwd);
    if !validation.valid {
        return Err(LspError::InvalidPath(format!(
            "[lsp] {}",
            validation.error.unwrap_or_default()
        )));
    }
    let command_processor = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string());
    let prepared = create_spawn_command(
        command,
        SpawnPlatform::current(),
        &command_processor,
        &options.env,
    )?;
    let mut builder = Command::new(&prepared.command);
    builder
        .args(&prepared.args)
        .current_dir(&options.cwd)
        .env_clear()
        .envs(&options.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    builder.process_group(0);
    let mut child = builder.spawn().map_err(|error| {
        LspError::ProcessSpawn(format!(
            "[lsp] failed to spawn {}: {error}",
            prepared.command
        ))
    })?;
    Ok(SpawnedProcess {
        pid: child.id(),
        stdin: child.stdin.take(),
        stdout: child.stdout.take(),
        stderr: child.stderr.take(),
        child,
    })
}

fn kill_process_tree(child: &mut Child, pid: Option<u32>, signal: KillSignal) {
    #[cfg(unix)]
    if let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) {
        let signo = match signal {
            KillSignal::Term => libc::SIGTERM,
            KillSignal::Kill => libc::SIGKILL,
        };
        // SAFETY: `kill` has no memory-safety preconditions; a negative pid targets the group.
        let result = unsafe { libc::kill(-pid, signo) };
        if result == 0 {
            return;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            report_best_effort_cleanup_error("process group kill", &error);
        }
    }
    #[cfg(not(unix))]
    let _ = (pid, signal);
    if let Err(error) = child.start_kill()
        && error.kind() != std::io::ErrorKind::InvalidInput
    {
        report_best_effort_cleanup_error("process kill", &error);
    }
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
