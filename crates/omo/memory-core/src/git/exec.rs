//! Git execution abstraction and process spawner.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

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

/// Raw result of a finished Git process whose stdout must survive byte-for-byte.
///
/// `git cat-file --batch` frames each record with a byte length, so decoding its stdout before
/// parsing would both shift every later record and destroy a non-UTF-8 blob body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitExecBytesResult {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl GitExecBytesResult {
    /// The lossy text view of `stdout`, matching what [`GitExecResult`] would have carried.
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// Abstraction over Git process execution for testing and platform shimming.
///
/// Implementations run `git` with `argv` in `options.cwd` and report the exit code and captured
/// output; a non-zero exit is a normal result, `Err` is reserved for spawn/IO failures.
pub trait GitExec: Send + Sync {
    fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult>;

    /// Run `git` and keep `stdout` as raw bytes.
    ///
    /// The default implementation falls back to the lossy text view, which is correct for every
    /// text read; only a caller that parses byte-framed output needs an implementation that keeps
    /// the bytes intact.
    fn run_bytes(
        &self,
        argv: &[String],
        options: &GitExecOptions,
    ) -> std::io::Result<GitExecBytesResult> {
        let result = self.run(argv, options)?;
        Ok(GitExecBytesResult {
            code: result.code,
            stdout: result.stdout.into_bytes(),
            stderr: result.stderr,
        })
    }
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

    /// Run `git` with string-slice arguments in `cwd`, keeping stdout as raw bytes.
    pub fn run_bytes_in(
        &self,
        cwd: &std::path::Path,
        args: &[&str],
    ) -> std::io::Result<GitExecBytesResult> {
        let argv: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        self.run_bytes(
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

    /// The `run_command` seam carries decoded text, so the byte-faithful path spawns the real `git`
    /// with the same Windows install-root fallback; only a caller parsing byte-framed output (the
    /// completion secret scan's `cat-file --batch`) needs it.
    fn run_bytes(
        &self,
        argv: &[String],
        options: &GitExecOptions,
    ) -> std::io::Result<GitExecBytesResult> {
        let err = match run_git_command_bytes("git", argv, options) {
            Ok(result) => return Ok(result),
            Err(err) => err,
        };
        if self.platform != GitPlatform::Windows || err.kind() != std::io::ErrorKind::NotFound {
            return Err(err);
        }
        for candidate in windows_git_candidates(options) {
            match run_git_command_bytes(&candidate, argv, options) {
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
    let (code, stdout, stderr) = run_git_command_raw(executable, argv, options)?;
    Ok(GitExecResult {
        code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn run_git_command_bytes(
    executable: &str,
    argv: &[String],
    options: &GitExecOptions,
) -> std::io::Result<GitExecBytesResult> {
    let (code, stdout, stderr) = run_git_command_raw(executable, argv, options)?;
    Ok(GitExecBytesResult {
        code,
        stdout,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn run_git_command_raw(
    executable: &str,
    argv: &[String],
    options: &GitExecOptions,
) -> std::io::Result<(i32, Vec<u8>, Vec<u8>)> {
    // A missing cwd behaves like git itself (exit 128), never like a missing binary.
    if !options.cwd.exists() {
        return Ok((
            128,
            Vec::new(),
            format!(
                "fatal: cannot change to '{}': No such file or directory",
                options.cwd.display()
            )
            .into_bytes(),
        ));
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

fn resolve_executable_path(executable: &str, custom_path: &str, cwd: &Path) -> Option<PathBuf> {
    if executable.contains('/') || executable.contains('\\') {
        let p = PathBuf::from(executable);
        return if p.is_absolute() {
            Some(p)
        } else {
            Some(cwd.join(p))
        };
    }
    for dir in std::env::split_paths(custom_path) {
        let candidate = dir.join(executable);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            for ext in [".exe", ".cmd", ".bat"] {
                let candidate_ext = dir.join(format!("{executable}{ext}"));
                if candidate_ext.is_file() {
                    return Some(candidate_ext);
                }
            }
        }
    }
    None
}

fn spawn_and_capture(
    executable: &str,
    argv: &[String],
    options: &GitExecOptions,
) -> std::io::Result<(i32, Vec<u8>, Vec<u8>)> {
    let explicit_path = options
        .env
        .iter()
        .find(|(k, v)| k.eq_ignore_ascii_case("PATH") && !v.trim().is_empty())
        .map(|(_, v)| v.clone());

    let resolved_exe = if let Some(ref path_var) = explicit_path {
        match resolve_executable_path(executable, path_var, &options.cwd) {
            Some(path) => path.to_string_lossy().into_owned(),
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("program not found: {executable}"),
                ));
            }
        }
    } else {
        executable.to_string()
    };

    let mut cmd = Command::new(&resolved_exe);
    cmd.args(argv);
    cmd.current_dir(&options.cwd);

    if !options.env.is_empty() {
        for (key, val) in &options.env {
            cmd.env(key, val);
        }
    }
    #[cfg(windows)]
    if let Some(ref path_var) = explicit_path {
        cmd.env("PATH", path_var);
        cmd.env("Path", path_var);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0");

    if options.stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let activity = super::git_activity_lock::hold_git_activity_shared(&options.cwd)?;
    #[cfg(unix)]
    if let Some(ref hold) = activity {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        let fd = hold.0.as_raw_fd();
        // SAFETY: only async-signal-safe descriptor operations run between fork and exec.
        #[allow(unsafe_code)]
        unsafe {
            cmd.pre_exec(move || {
                if fd != 3 && libc::dup2(fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    // The shared activity hold must outlive spawn; only Unix also hands it to the child.
    #[cfg(not(unix))]
    let _ = &activity;
    let mut child = cmd.spawn()?;

    let mut stdout_handle = child.stdout.take();
    let mut stderr_handle = child.stderr.take();

    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();

    let stdout_buffer = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stderr_buffer = Arc::new(std::sync::Mutex::new(Vec::new()));
    let out_buffer = Arc::clone(&stdout_buffer);
    let err_buffer = Arc::clone(&stderr_buffer);
    let out_thread = std::thread::spawn(move || {
        if let Some(mut stream) = stdout_handle.take() {
            let mut chunk = [0; 8192];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(size) => out_buffer
                        .lock()
                        .expect("stdout buffer")
                        .extend_from_slice(&chunk[..size]),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
        let _ = out_tx.send(());
    });

    let err_thread = std::thread::spawn(move || {
        if let Some(mut stream) = stderr_handle.take() {
            let mut chunk = [0; 8192];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(size) => err_buffer
                        .lock()
                        .expect("stderr buffer")
                        .extend_from_slice(&chunk[..size]),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
        let _ = err_tx.send(());
    });

    // Write stdin from its own thread after the readers are running so a child that fills its
    // stdout pipe before draining stdin (e.g. `cat-file --batch`) cannot deadlock us.
    let (stdin_tx, stdin_rx) = mpsc::channel();
    let stdin_thread = match (options.stdin.clone(), child.stdin.take()) {
        (Some(stdin_bytes), Some(mut stdin)) => Some(std::thread::spawn(move || {
            let result = stdin.write_all(&stdin_bytes).and_then(|()| stdin.flush());
            let _ = stdin_tx.send(result);
        })),
        _ => None,
    };

    let timeout_ms = options.timeout_ms;
    let status = if timeout_ms == 0 {
        child.wait()?
    } else {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut backoff = Duration::from_micros(200);
        loop {
            match child.try_wait()? {
                Some(status) => break status,
                None => {
                    if Instant::now() >= deadline {
                        #[cfg(unix)]
                        {
                            // SAFETY: child.id() is the live child owned by this function.
                            #[allow(unsafe_code)]
                            unsafe {
                                libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
                            }
                            let grace = Instant::now() + Duration::from_secs(10);
                            while child.try_wait()?.is_none() && Instant::now() < grace {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                        }
                        if child.try_wait()?.is_none() {
                            child.kill()?;
                        }
                        child.wait()?;
                        drop(stdin_thread);
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!("git {} timed out after {timeout_ms}ms", argv.join(" ")),
                        ));
                    }
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(5));
                }
            }
        }
    };

    // EOF events have a common bound: descendants may inherit the pipes indefinitely.
    let deadline = Instant::now() + Duration::from_secs(2);
    let stdin_error = if stdin_thread.is_some() {
        match stdin_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Err(error)) if error.kind() != std::io::ErrorKind::BrokenPipe => {
                Some(error.to_string())
            }
            _ => None,
        }
    } else {
        None
    };
    let _ = out_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    let _ = err_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    let stdout_bytes = stdout_buffer.lock().expect("stdout buffer").clone();
    let stderr_bytes = stderr_buffer.lock().expect("stderr buffer").clone();
    drop(out_thread);
    drop(err_thread);

    let mut stderr_bytes = stderr_bytes;
    if let Some(error) = stdin_error {
        stderr_bytes.extend_from_slice(format!("\nstdin: {error}").as_bytes());
    }

    Ok((status.code().unwrap_or(1), stdout_bytes, stderr_bytes))
}
