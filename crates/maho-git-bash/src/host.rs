//! The machine the server runs on, behind a trait so tests can inject every input.

use std::path::PathBuf;

/// One `run` invocation, after argument validation and Git Bash lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRequest {
    /// Full path of the `bash.exe` to start.
    pub bash: String,
    /// Trimmed command text, passed as `-lc <command>`.
    pub command: String,
    /// Working directory; `None` keeps the server's own cwd.
    pub workdir: Option<String>,
    /// Timeout in milliseconds (1..=1800000).
    pub timeout_ms: u64,
}

/// What a finished (or timed-out) command produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOutput {
    /// Process exit code, `None` when the system reports none.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Inputs and effects the server needs from the machine.
pub trait Host: Send + Sync {
    /// Node-style platform name (`win32`, `linux`, `darwin`, ...).
    fn platform(&self) -> String;
    /// An environment variable, `None` when unset or not Unicode.
    fn env_var(&self, name: &str) -> Option<String>;
    /// Whether `path` exists.
    fn file_exists(&self, path: &str) -> bool;
    /// Standard output of `where bash` (empty when the command fails).
    fn where_bash(&self) -> String;
    /// Run a command through Git Bash. `Err` carries the system error text when the process
    /// could not be started or awaited.
    fn run(
        &self,
        request: RunRequest,
    ) -> impl std::future::Future<Output = Result<RunOutput, String>> + Send;
}

/// Node-style name of the platform this binary was built for.
pub fn node_platform() -> String {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
    .to_owned()
}

/// The real machine.
#[derive(Clone, Debug, Default)]
pub struct SystemHost {
    /// Parent directory for the per-call output directories; `None` uses the system temp dir.
    pub temp_root: Option<PathBuf>,
}

impl Host for SystemHost {
    fn platform(&self) -> String {
        node_platform()
    }

    fn env_var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn file_exists(&self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }

    fn where_bash(&self) -> String {
        let mut command = std::process::Command::new("where");
        command
            .arg("bash")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(crate::runner::CREATE_NO_WINDOW);
        }
        match command.output() {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).into_owned()
            }
            _ => String::new(),
        }
    }

    async fn run(&self, request: RunRequest) -> Result<RunOutput, String> {
        crate::runner::run_command(&request, self.temp_root.as_deref()).await
    }
}
