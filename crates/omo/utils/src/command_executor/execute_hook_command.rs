use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use super::home_directory::get_home_directory;
use super::shell_path::{find_bash_path, find_zsh_path};

const DEFAULT_HOOK_TIMEOUT_MS: u64 = 30_000;
const SIGKILL_GRACE_MS: u64 = 5_000;
const TIMEOUT_EXIT_CODE: i32 = 124;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub exit_code: i32,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecuteHookOptions {
    pub force_zsh: bool,
    pub zsh_path: Option<String>,
    /// Kill deadline in milliseconds (default 30000).
    pub timeout_ms: Option<u64>,
    /// Grace between SIGTERM and SIGKILL after a timeout (default 5000).
    pub kill_grace_ms: Option<u64>,
    /// When set, the child env is scrubbed to these vars plus HOME/PATH/CLAUDE_PROJECT_DIR.
    pub allowed_env_vars: Option<Vec<String>>,
    /// Exported as CLAUDE_PLUGIN_ROOT and substituted into the command string.
    pub plugin_root: Option<String>,
}

fn expand_command(command: &str, home: &str, cwd: &str, plugin_root: Option<&str>) -> String {
    let mut expanded = match command.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ => command.to_string(),
    };
    let mut spaced = String::with_capacity(expanded.len());
    let mut chars = expanded.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch.is_whitespace() && expanded[index + ch.len_utf8()..].starts_with("~/") {
            spaced.push(' ');
            spaced.push_str(home);
            chars.next();
            continue;
        }
        spaced.push(ch);
    }
    expanded = spaced
        .replace("$CLAUDE_PROJECT_DIR", cwd)
        .replace("${CLAUDE_PROJECT_DIR}", cwd);
    if let Some(root) = plugin_root {
        expanded = expanded
            .replace("$CLAUDE_PLUGIN_ROOT", root)
            .replace("${CLAUDE_PLUGIN_ROOT}", root);
    }
    expanded
}

fn build_env(home: &str, cwd: &str, options: &ExecuteHookOptions) -> HashMap<String, String> {
    const PROTECTED_ENV_KEYS: [&str; 3] = ["HOME", "CLAUDE_PROJECT_DIR", "CLAUDE_PLUGIN_ROOT"];
    let mut env: HashMap<String, String> = match &options.allowed_env_vars {
        Some(allowed) => {
            let allowed: HashSet<&str> = allowed.iter().map(String::as_str).collect();
            let mut scrubbed: HashMap<String, String> = std::env::vars()
                .filter(|(key, _)| {
                    allowed.contains(key.as_str()) && !PROTECTED_ENV_KEYS.contains(&key.as_str())
                })
                .collect();
            if let Ok(path) = std::env::var("PATH") {
                scrubbed.insert("PATH".to_string(), path);
            }
            scrubbed
        }
        None => std::env::vars().collect(),
    };
    env.insert("HOME".to_string(), home.to_string());
    env.insert("CLAUDE_PROJECT_DIR".to_string(), cwd.to_string());
    if let Some(root) = &options.plugin_root {
        env.insert("CLAUDE_PLUGIN_ROOT".to_string(), root.clone());
    }
    env
}

fn shell_command(command: &str) -> Command {
    if cfg!(windows) {
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/d", "/s", "/c", command]);
        cmd
    } else {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", command]);
        cmd
    }
}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn detach(_command: &mut Command) {}

#[cfg(unix)]
fn signal_group(child_pid: u32, signal: libc::c_int) {
    let Ok(pid) = libc::pid_t::try_from(child_pid) else {
        return;
    };
    // SAFETY: kill(2) takes plain integers; a negative pid targets the process group we created.
    let group_result = unsafe { libc::kill(-pid, signal) };
    if group_result != 0 {
        // SAFETY: same as above, targeting only the direct child.
        unsafe { libc::kill(pid, signal) };
    }
}

#[cfg(not(unix))]
fn kill_windows_tree(child_pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &child_pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn spawn_reader(
    stream: Option<impl Read + Send + 'static>,
    sink: Arc<Mutex<Vec<u8>>>,
) -> Option<thread::JoinHandle<()>> {
    let mut stream = stream?;
    Some(thread::spawn(move || {
        let mut chunk = [0_u8; 8192];
        while let Ok(read) = stream.read(&mut chunk) {
            if read == 0 {
                break;
            }
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(&chunk[..read]);
        }
    }))
}

fn text_of(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buffer.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
}

fn finish(exit_code: i32, stdout: &str, stderr: &str) -> CommandResult {
    CommandResult {
        exit_code,
        stdout: Some(stdout.trim().to_string()),
        stderr: Some(stderr.trim().to_string()),
    }
}

/// Run a hook through the shell with stdin, env shaping, and a process-group timeout (exit 124).
pub fn execute_hook_command(
    command: &str,
    stdin: &str,
    cwd: &str,
    options: &ExecuteHookOptions,
) -> CommandResult {
    let home = get_home_directory();
    let timeout_ms = options.timeout_ms.unwrap_or(DEFAULT_HOOK_TIMEOUT_MS);
    let kill_grace_ms = options.kill_grace_ms.unwrap_or(SIGKILL_GRACE_MS);
    let expanded = expand_command(command, &home, cwd, options.plugin_root.as_deref());
    let mut final_command = expanded.clone();
    if options.force_zsh {
        let escaped = expanded.replace('\'', "'\\''");
        if let Some(shell) = find_zsh_path(options.zsh_path.as_deref()).or_else(find_bash_path) {
            final_command = format!("{shell} -lc '{escaped}'");
        }
    }

    let mut process = shell_command(&final_command);
    process
        .current_dir(cwd)
        .env_clear()
        .envs(build_env(&home, cwd, options))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    detach(&mut process);
    let mut child: Child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CommandResult {
                exit_code: 1,
                stdout: None,
                stderr: Some(error.to_string()),
            };
        }
    };
    let pid = child.id();
    if let Some(mut child_stdin) = child.stdin.take() {
        let _ = child_stdin.write_all(stdin.as_bytes());
    }
    let stdout = Arc::new(Mutex::new(Vec::new()));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    let readers = [
        spawn_reader(child.stdout.take(), Arc::clone(&stdout)),
        spawn_reader(child.stderr.take(), Arc::clone(&stderr)),
    ];
    let (closed_tx, closed_rx) = mpsc::channel();
    thread::spawn(move || {
        let status = child.wait();
        for reader in readers.into_iter().flatten() {
            let _ = reader.join();
        }
        let _ = closed_tx.send(status.ok().and_then(|status| status.code()));
    });

    match closed_rx.recv_timeout(Duration::from_millis(timeout_ms)) {
        Ok(code) => return finish(code.unwrap_or(1), &text_of(&stdout), &text_of(&stderr)),
        Err(RecvTimeoutError::Disconnected) => {
            return finish(1, &text_of(&stdout), &text_of(&stderr));
        }
        Err(RecvTimeoutError::Timeout) => {}
    }

    let notice = format!("\nHook command timed out after {timeout_ms}ms");
    #[cfg(unix)]
    {
        signal_group(pid, libc::SIGTERM);
        if closed_rx
            .recv_timeout(Duration::from_millis(kill_grace_ms))
            .is_err()
        {
            signal_group(pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = kill_grace_ms;
        kill_windows_tree(pid);
    }
    let mut stderr_text = text_of(&stderr);
    if !stderr_text.contains("Hook command timed out after") {
        stderr_text.push_str(&notice);
    }
    finish(TIMEOUT_EXIT_CODE, &text_of(&stdout), &stderr_text)
}
