//! Git execution abstraction and process spawner.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// Execution options passed to Git commands.
#[derive(Debug, Clone, Default)]
pub struct GitExecOptions {
    pub cwd: PathBuf,
    pub timeout_ms: u64,
    pub env: BTreeMap<String, String>,
    pub stdin: Option<Vec<u8>>,
}

/// Raw result of a finished Git process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitExecResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Abstraction over Git process execution for testing and platform shimming.
///
/// Implementations run `git` with `argv` in `options.cwd` and report the exit code and captured
/// output; a non-zero exit is a normal result, `Err` is reserved for spawn/IO failures.
pub trait GitExec: Send + Sync {
    fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult>;
}

impl dyn GitExec + '_ {
    /// Run `git` with string-slice arguments in `cwd` using the default 30s timeout.
    pub fn run_in(&self, cwd: &std::path::Path, args: &[&str]) -> std::io::Result<GitExecResult> {
        let argv: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        self.run(
            &argv,
            &GitExecOptions {
                cwd: cwd.to_path_buf(),
                timeout_ms: 30_000,
                ..GitExecOptions::default()
            },
        )
    }
}

/// Target platform that decides whether Windows Git install roots are probed as fallbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitPlatform {
    Windows,
    Other,
}

impl GitPlatform {
    /// Platform of the running process.
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// Spawns `executable` with `argv`; the seam `create_git_exec` drives (TS `runCommand`).
pub type GitRunCommand =
    dyn Fn(&str, &[String], &GitExecOptions) -> std::io::Result<GitExecResult> + Send + Sync;

/// Injectable runtime for [`create_git_exec`] (TS `NodeGitExecRuntime`).
#[derive(Default)]
pub struct GitExecRuntime {
    pub platform: Option<GitPlatform>,
    pub run_command: Option<Arc<GitRunCommand>>,
}

struct RuntimeGitExec {
    platform: GitPlatform,
    run_command: Arc<GitRunCommand>,
}

impl GitExec for RuntimeGitExec {
    fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        let err = match (self.run_command)("git", argv, options) {
            Ok(result) => return Ok(result),
            Err(err) => err,
        };
        // Only a missing binary (ENOENT) on Windows falls back to standard install roots.
        if self.platform != GitPlatform::Windows || err.kind() != std::io::ErrorKind::NotFound {
            return Err(err);
        }
        for candidate in windows_git_candidates(options) {
            match (self.run_command)(&candidate, argv, options) {
                Ok(result) => return Ok(result),
                Err(fallback) if fallback.kind() == std::io::ErrorKind::NotFound => {}
                Err(fallback) => return Err(fallback),
            }
        }
        Err(err)
    }
}

/// Builds a Git executor over `runtime`, defaulting to the current platform and a real spawner.
pub fn create_git_exec(runtime: GitExecRuntime) -> Arc<dyn GitExec> {
    Arc::new(RuntimeGitExec {
        platform: runtime.platform.unwrap_or_else(GitPlatform::current),
        run_command: runtime
            .run_command
            .unwrap_or_else(|| Arc::new(run_git_command)),
    })
}

/// Returns the system Git execution engine using standard process spawning.
pub fn system_git_exec() -> Arc<dyn GitExec> {
    create_git_exec(GitExecRuntime::default())
}

const WINDOWS_GIT_CANDIDATES: &[(&str, &[&str])] = &[
    ("ProgramFiles", &["Git", "cmd", "git.exe"]),
    ("ProgramW6432", &["Git", "cmd", "git.exe"]),
    ("ProgramFiles(x86)", &["Git", "cmd", "git.exe"]),
    ("LOCALAPPDATA", &["Programs", "Git", "cmd", "git.exe"]),
];

fn run_git_command(
    executable: &str,
    argv: &[String],
    options: &GitExecOptions,
) -> std::io::Result<GitExecResult> {
    // A missing cwd behaves like git itself (exit 128), never like a missing binary.
    if !options.cwd.exists() {
        return Ok(GitExecResult {
            code: 128,
            stdout: String::new(),
            stderr: format!(
                "fatal: cannot change to '{}': No such file or directory",
                options.cwd.display()
            ),
        });
    }
    spawn_and_capture(executable, argv, options)
}

fn environment_value(options: &GitExecOptions, name: &str) -> Option<String> {
    let value = if options.env.is_empty() {
        std::env::vars()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    } else {
        options
            .env
            .iter()
            .find(|(k, v)| k.eq_ignore_ascii_case(name) && !v.trim().is_empty())
            .map(|(_, v)| v.clone())
    }?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn windows_git_candidates(options: &GitExecOptions) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut candidates = Vec::new();

    for (env_key, subparts) in WINDOWS_GIT_CANDIDATES {
        let Some(root) = environment_value(options, env_key) else {
            continue;
        };
        let bytes = root.as_bytes();
        // Only a drive-qualified local root (`C:\\` or `C:/`) is trusted.
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || (bytes[2] != b'\\' && bytes[2] != b'/')
        {
            continue;
        }
        let normalized_root = root.replace('/', "\\");
        let mut candidate = normalized_root.trim_end_matches('\\').to_string();
        for part in *subparts {
            candidate.push('\\');
            candidate.push_str(part);
        }
        if seen.insert(candidate.to_lowercase()) {
            candidates.push(candidate);
        }
    }

    candidates
}

fn spawn_and_capture(
    executable: &str,
    argv: &[String],
    options: &GitExecOptions,
) -> std::io::Result<GitExecResult> {
    let mut cmd = Command::new(executable);
    cmd.args(argv);
    cmd.current_dir(&options.cwd);

    if !options.env.is_empty() {
        for (key, val) in &options.env {
            cmd.env(key, val);
        }
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0");

    if options.stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;

    if let Some(stdin_bytes) = &options.stdin
        && let Some(mut stdin) = child.stdin.take()
    {
        let _ = stdin.write_all(stdin_bytes);
        let _ = stdin.flush();
    }

    let mut stdout_handle = child.stdout.take();
    let mut stderr_handle = child.stderr.take();

    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();

    let out_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut stream) = stdout_handle.take() {
            let _ = stream.read_to_end(&mut bytes);
        }
        let _ = out_tx.send(bytes);
    });

    let err_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut stream) = stderr_handle.take() {
            let _ = stream.read_to_end(&mut bytes);
        }
        let _ = err_tx.send(bytes);
    });

    let timed_out = Arc::new(AtomicBool::new(false));
    let timeout_ms = options.timeout_ms;

    let (watchdog_tx, watchdog_rx) = mpsc::channel();
    let timed_out_clone = Arc::clone(&timed_out);

    let watchdog = if timeout_ms > 0 {
        Some(std::thread::spawn(move || {
            if watchdog_rx
                .recv_timeout(Duration::from_millis(timeout_ms))
                .is_err()
            {
                timed_out_clone.store(true, Ordering::SeqCst);
            }
        }))
    } else {
        None
    };

    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None => {
                if timed_out.load(Ordering::SeqCst) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("git {} timed out after {timeout_ms}ms", argv.join(" ")),
                    ));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };

    if let Some(w_tx) = Some(watchdog_tx) {
        let _ = w_tx.send(());
    }
    if let Some(w) = watchdog {
        let _ = w.join();
    }

    let _ = out_thread.join();
    let _ = err_thread.join();

    let stdout_bytes = out_rx.recv().unwrap_or_default();
    let stderr_bytes = err_rx.recv().unwrap_or_default();

    let code = status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();

    Ok(GitExecResult {
        code,
        stdout,
        stderr,
    })
}
