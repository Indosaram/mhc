//! Port of `packages/git-bash-mcp/src/runner.ts`.
//!
//! Runs a command through Git Bash (`bash -lc <command>`) and captures stdout/stderr into per-call
//! temp files, mirroring the reference implementation's temp-fd capture that avoids Windows
//! pipe-buffer deadlocks.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use tempfile::Builder;
use tempfile::TempDir;

/// How long the run loop waits between child-status checks.
///
/// The reference implementation parks on the child's `close` event and kills the child from a
/// `setTimeout`; `std::process::Child` has no wait-with-timeout and this crate forbids `unsafe`, so
/// the port observes the same two outcomes (exit, or timeout kill) by polling `try_wait`.
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Temp-file prefix, matching the reference `mkdtempSync(join(tmpdir(), ...))`.
const OUTPUT_DIRECTORY_PREFIX: &str = "omo-git-bash-run-";

/// Inputs for [`run_git_bash_command`]; mirrors the reference `GitBashRunInput`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBashRunInput {
    pub bash_path: String,
    pub command: String,
    pub cwd: Option<String>,
    pub timeout_ms: u64,
    /// Replaces the whole child environment when set, as Node's `spawn({ env })` does.
    pub env: Option<HashMap<String, String>>,
}

/// The reference `GitBashRunResult`; `exit_code` is `None` when the child died from a signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBashRunResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// The command-runner seam; `Err` carries the message the handler returns as `isError` text.
pub type RunGitBashCommand =
    dyn Fn(&GitBashRunInput) -> Result<GitBashRunResult, String> + Send + Sync;

/// The per-call temp directory holding the captured stdout/stderr; removed when dropped.
struct OutputCapture {
    _directory: TempDir,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

impl OutputCapture {
    fn create_in(base: &Path) -> io::Result<Self> {
        let directory = Builder::new()
            .prefix(OUTPUT_DIRECTORY_PREFIX)
            .tempdir_in(base)?;
        let stdout_path = directory.path().join("stdout");
        let stderr_path = directory.path().join("stderr");
        Ok(Self {
            _directory: directory,
            stdout_path,
            stderr_path,
        })
    }

    fn open_stdout(&self) -> io::Result<File> {
        File::create(&self.stdout_path)
    }

    fn open_stderr(&self) -> io::Result<File> {
        File::create(&self.stderr_path)
    }

    /// Reads both captures with the reference's lossy UTF-8 decode.
    fn read(&self) -> io::Result<(String, String)> {
        Ok((
            read_lossy(&self.stdout_path)?,
            read_lossy(&self.stderr_path)?,
        ))
    }
}

fn read_lossy(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Runs `bash -lc <command>` capturing stdout/stderr to temp files under the system temp dir.
///
/// # Errors
/// Returns the spawn/IO error when the bash process cannot start or its capture cannot be read.
pub fn run_git_bash_command(input: &GitBashRunInput) -> io::Result<GitBashRunResult> {
    run_git_bash_command_in_dir(input, &std::env::temp_dir())
}

/// [`run_git_bash_command`] against an explicit output base.
///
/// The reference always uses the system temp dir; this seam makes the per-call output-directory
/// cleanup observable in a deterministic test.
#[doc(hidden)]
pub fn run_git_bash_command_in_dir(
    input: &GitBashRunInput,
    base: &Path,
) -> io::Result<GitBashRunResult> {
    let capture = OutputCapture::create_in(base)?;
    let stdout_file = capture.open_stdout()?;
    let stderr_file = capture.open_stderr()?;

    let mut command = Command::new(&input.bash_path);
    command.arg("-lc").arg(&input.command);
    if let Some(cwd) = &input.cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = &input.env {
        command.env_clear().envs(env);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_millis(input.timeout_ms);
    let mut timed_out = false;
    let status: ExitStatus = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            // Best effort: a kill failure means the child already exited, which `wait` reports.
            let _kill = child.kill();
            break child.wait()?;
        }
        thread::sleep(STATUS_POLL_INTERVAL);
    };
    let (stdout, stderr) = capture.read()?;
    Ok(GitBashRunResult {
        exit_code: status.code(),
        stdout,
        stderr,
        timed_out,
    })
}
