use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::backend::{IsolationError, Result};
use crate::util::lock;

const DEFAULT_GIT_TIMEOUT_MS: u64 = 120_000;
const PIPE_DRAIN_GRACE_MS: u64 = 1_000;

pub fn exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub type OutputLimitError = Arc<dyn Fn() -> IsolationError + Send + Sync>;
pub type SpawnObserver = Arc<dyn Fn(u32) + Send + Sync>;

#[derive(Clone, Default)]
pub struct GitOptions {
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// Maximum command lifetime before the full Git process tree is terminated.
    pub timeout_ms: Option<u64>,
    pub input: Option<Vec<u8>>,
    pub allowed_exit_codes: Option<Vec<i32>>,
    pub max_output_bytes: Option<u64>,
    pub output_limit_error: Option<OutputLimitError>,
    /// Observes the spawned process (tests use it to signal git by pid).
    pub on_spawn: Option<SpawnObserver>,
}

impl GitOptions {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        GitOptions {
            cwd: cwd.into(),
            ..Default::default()
        }
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    pub fn timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    pub fn input(mut self, input: Vec<u8>) -> Self {
        self.input = Some(input);
        self
    }

    pub fn allowed_exit_codes(mut self, codes: Vec<i32>) -> Self {
        self.allowed_exit_codes = Some(codes);
        self
    }
}

#[derive(Debug, Clone)]
pub struct GitOutput {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

pub fn kill_tree(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    {
        let group = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        if group != 0 {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/pid", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
    }
}

#[derive(Clone)]
struct OutputLimit {
    retained: Arc<AtomicU64>,
    max_output_bytes: Option<u64>,
    limit_error: Arc<Mutex<Option<IsolationError>>>,
    output_limit_error: Option<OutputLimitError>,
}

fn collect(
    mut stream: Box<dyn Read + Send>,
    limit: OutputLimit,
    pid: u32,
    sink: Arc<Mutex<Vec<u8>>>,
    done: Sender<()>,
) {
    let OutputLimit { retained, max_output_bytes, limit_error, output_limit_error } = limit;
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                let total = retained.fetch_add(read as u64, Ordering::SeqCst) + read as u64;
                if let Some(max) = max_output_bytes && total > max {
                        let error = match &output_limit_error {
                            Some(factory) => factory(),
                            None => IsolationError::other("Git output exceeds budget"),
                        };
                        *lock(&limit_error) = Some(error);
                        kill_tree(pid);
                        break;
                }
                lock(&sink).extend_from_slice(&chunk[..read]);
            }
            Err(_) => break,
        }
    }
    let _ = done.send(());
}

pub fn run_git(args: &[String], options: &GitOptions) -> Result<GitOutput> {
    run_git_with_input(args, options, options.input.as_deref().map(Arc::<[u8]>::from))
}

/// [`run_git`] with stdin bytes shared by reference count instead of copied per call;
/// `options.input` is ignored.
pub(crate) fn run_git_with_input(
    args: &[String],
    options: &GitOptions,
    input: Option<Arc<[u8]>>,
) -> Result<GitOutput> {
    #[cfg(windows)]
    let git_program = {
        let custom_path = options.env.iter().find_map(|(k, v)| {
            if k.eq_ignore_ascii_case("PATH") {
                Some(v.as_str())
            } else {
                None
            }
        });
        if let Some(path_val) = custom_path {
            let found_exe = std::env::split_paths(path_val).find_map(|dir| {
                [".exe", ".cmd", ".bat", ""].iter().find_map(|ext| {
                    let candidate = dir.join(format!("git{ext}"));
                    if candidate.is_file() {
                        Some(candidate)
                    } else {
                        None
                    }
                })
            });
            match found_exe {
                Some(path) => path,
                None => return Err(IsolationError::unavailable("git not on PATH")),
            }
        } else {
            PathBuf::from("git")
        }
    };
    #[cfg(not(windows))]
    let git_program = PathBuf::from("git");

    let mut command = Command::new(&git_program);
    command.args(args);
    command.current_dir(&options.cwd);
    for (key, value) in &options.env {
        command.env(key, value);
        #[cfg(windows)]
        if key.eq_ignore_ascii_case("PATH") {
            command.env("Path", value);
            command.env("PATH", value);
        }
    }
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(IsolationError::unavailable("git not on PATH"));
        }
        #[cfg(windows)]
        Err(error) if error.raw_os_error() == Some(2) || error.raw_os_error() == Some(3) => {
            return Err(IsolationError::unavailable("git not on PATH"));
        }
        Err(error) => return Err(error.into()),
    };
    let pid = child.id();
    if let Some(on_spawn) = &options.on_spawn {
        on_spawn(pid);
    }
    if let Some(input) = input && let Some(mut stdin) = child.stdin.take() {
            std::thread::spawn(move || {
                if stdin.write_all(&input).is_err() {
                    kill_tree(pid);
                    return;
                }
                let _ = stdin.flush();
            });
    }
    let retained = Arc::new(AtomicU64::new(0));
    let limit_error: Arc<Mutex<Option<IsolationError>>> = Arc::new(Mutex::new(None));
    let stdout_sink = Arc::new(Mutex::new(Vec::new()));
    let stderr_sink = Arc::new(Mutex::new(Vec::new()));
    let (done_tx, done_rx) = channel::<()>();
    let output_limit = OutputLimit {
        retained: Arc::clone(&retained),
        max_output_bytes: options.max_output_bytes,
        limit_error: Arc::clone(&limit_error),
        output_limit_error: options.output_limit_error.clone(),
    };
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn({
            let sink = Arc::clone(&stdout_sink);
            let limit = output_limit.clone();
            let done = done_tx.clone();
            move || collect(Box::new(stdout), limit, pid, sink, done)
        });
    } else {
        let _ = done_tx.send(());
    }
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn({
            let sink = Arc::clone(&stderr_sink);
            let limit = output_limit.clone();
            let done = done_tx.clone();
            move || collect(Box::new(stderr), limit, pid, sink, done)
        });
    } else {
        let _ = done_tx.send(());
    }
    drop(done_tx);
    let timeout_ms = options.timeout_ms.unwrap_or(DEFAULT_GIT_TIMEOUT_MS);
    let (exit_tx, exit_rx) = channel::<std::io::Result<std::process::ExitStatus>>();
    std::thread::spawn(move || {
        let _ = exit_tx.send(child.wait());
    });
    let status = match exit_rx.recv_timeout(Duration::from_millis(timeout_ms)) {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => return Err(error.into()),
        Err(_) => {
            kill_tree(pid);
            let _ = exit_rx.recv_timeout(Duration::from_secs(5));
            let _ = done_rx.recv_timeout(Duration::from_millis(PIPE_DRAIN_GRACE_MS));
            let _ = done_rx.recv_timeout(Duration::from_millis(PIPE_DRAIN_GRACE_MS));
            return Err(IsolationError::GitTimeout {
                args: args.to_vec(),
                cwd: options.cwd.clone(),
                timeout_ms,
                message: format!("git timed out after {timeout_ms}ms"),
            });
        }
    };
    let allowed = options.allowed_exit_codes.clone().unwrap_or_else(|| vec![0]);
    let signal = exit_signal(&status);
    let code = status.code();
    let failed = signal.is_some() || !allowed.contains(&code.unwrap_or(0));
    // A child that exits to a signal death or a disallowed code has already
    // failed, so the tree is killed at exit; a grandchild may still hold the
    // pipes, so the collectors are given a bounded drain grace instead of
    // blocking the run past its own child's death.
    let grace = if failed {
        PIPE_DRAIN_GRACE_MS
    } else {
        timeout_ms
    };
    let _ = done_rx.recv_timeout(Duration::from_millis(grace));
    let _ = done_rx.recv_timeout(Duration::from_millis(grace));
    if failed {
        kill_tree(pid);
    }
    let stdout = std::mem::take(&mut *lock(&stdout_sink));
    let stderr_bytes = std::mem::take(&mut *lock(&stderr_sink));
    let stderr = String::from_utf8(stderr_bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
    if let Some(error) = lock(&limit_error).take() {
        return Err(error);
    }
    if let Some(signal) = signal {
        return Err(IsolationError::Git {
            args: args.to_vec(),
            cwd: options.cwd.clone(),
            code: 128,
            stderr: format!("git terminated by signal {signal}"),
            message: format!("git terminated by signal {signal}"),
        });
    }
    let code = code.unwrap_or(0);
    if !allowed.contains(&code) {
        return Err(IsolationError::Git {
            args: args.to_vec(),
            cwd: options.cwd.clone(),
            code,
            stderr: stderr.clone(),
            message: format!("git {} failed ({code}): {stderr}", args.join(" ")),
        });
    }
    Ok(GitOutput {
        code,
        stdout,
        stderr,
    })
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

pub fn git_result(cwd: &Path, args: &[String], input: Option<Vec<u8>>) -> Result<GitOutput> {
    let mut options = GitOptions::new(cwd.to_path_buf());
    options.input = input;
    options.allowed_exit_codes = Some((0..256).collect());
    run_git(args, &options)
}

pub fn git(cwd: &Path, args: &[String], input: Option<Vec<u8>>) -> Result<Vec<u8>> {
    let mut options = GitOptions::new(cwd.to_path_buf());
    options.input = input;
    Ok(run_git(args, &options)?.stdout)
}

pub fn git_text(cwd: &Path, args: &[String]) -> Result<String> {
    let stdout = git(cwd, args, None)?;
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
}

pub fn str_args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}
