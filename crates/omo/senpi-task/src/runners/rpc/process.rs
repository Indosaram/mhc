//! A spawned RPC child process (the Node `ChildProcess` seam of the TypeScript runner).
//!
//! A reaper thread owns the OS child and records its exit, so terminate, the protocol client and
//! the handle can all observe the exit without holding `&mut Child`.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// `RpcSpawnDescriptor`: the exact command line for one RPC child. `env` is the complete child env.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RpcSpawnDescriptor {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildExitStatus {
    pub code: Option<i32>,
    /// Terminating signal number (Unix only).
    pub signal: Option<i32>,
}

type ExitCell = Arc<(Mutex<Option<ChildExitStatus>>, Condvar)>;

pub struct RpcChildProcess {
    pid: Option<u32>,
    spawn_error: Option<String>,
    stdin: Mutex<Option<ChildStdin>>,
    stdout: Mutex<Option<ChildStdout>>,
    stderr: Mutex<Option<ChildStderr>>,
    exit: ExitCell,
}

impl RpcChildProcess {
    /// Spawn a descriptor detached into its own process group (never through a shell).
    pub fn spawn(descriptor: &RpcSpawnDescriptor) -> Self {
        let mut command = Command::new(&descriptor.command);
        command
            .args(&descriptor.args)
            .current_dir(&descriptor.cwd)
            .env_clear()
            .envs(&descriptor.env);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        Self::spawn_command(command)
    }

    /// Spawn a prepared command with all three stdio streams piped. A spawn failure yields an
    /// already-exited child carrying the error (Node's `error` event).
    pub fn spawn_command(mut command: Command) -> Self {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match command.spawn() {
            Ok(child) => Self::from_child(child),
            Err(error) => Self {
                pid: None,
                spawn_error: Some(error.to_string()),
                stdin: Mutex::new(None),
                stdout: Mutex::new(None),
                stderr: Mutex::new(None),
                exit: Arc::new((
                    Mutex::new(Some(ChildExitStatus {
                        code: None,
                        signal: None,
                    })),
                    Condvar::new(),
                )),
            },
        }
    }

    fn from_child(mut child: Child) -> Self {
        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let exit: ExitCell = Arc::new((Mutex::new(None), Condvar::new()));
        let reaper_exit = Arc::clone(&exit);
        thread::spawn(move || {
            let status = child.wait().map_or(
                ChildExitStatus {
                    code: None,
                    signal: None,
                },
                |status| ChildExitStatus {
                    code: status.code(),
                    signal: exit_signal(&status),
                },
            );
            let (cell, ready) = &*reaper_exit;
            *cell.lock().unwrap_or_else(PoisonError::into_inner) = Some(status);
            ready.notify_all();
        });
        Self {
            pid: Some(pid),
            spawn_error: None,
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            stderr: Mutex::new(stderr),
            exit,
        }
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn spawn_error(&self) -> Option<&str> {
        self.spawn_error.as_deref()
    }

    pub fn take_stdout(&self) -> Option<ChildStdout> {
        self.stdout
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    pub fn take_stderr(&self) -> Option<ChildStderr> {
        self.stderr
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Write one newline-terminated line to the child's stdin.
    pub fn write_line(&self, line: &str) -> io::Result<()> {
        let mut stdin = self.stdin.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(stdin) = stdin.as_mut() else {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "stdin is closed"));
        };
        stdin.write_all(format!("{line}\n").as_bytes())?;
        stdin.flush()
    }

    pub fn exit_status(&self) -> Option<ChildExitStatus> {
        *self.exit.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn has_exited(&self) -> bool {
        self.exit_status().is_some()
    }

    pub fn wait_exit(&self) -> ChildExitStatus {
        let (cell, ready) = &*self.exit;
        let mut status = cell.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(status) = *status {
                return status;
            }
            status = ready.wait(status).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Wait up to `timeout` for the exit; `None` when the child is still running.
    pub fn wait_exit_timeout(&self, timeout: Duration) -> Option<ChildExitStatus> {
        let deadline = Instant::now() + timeout;
        let (cell, ready) = &*self.exit;
        let mut status = cell.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if status.is_some() {
                return *status;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            status = ready
                .wait_timeout(status, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
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

/// Node-style signal name (`SIGTERM`) for a terminating signal number.
pub fn signal_name(signal: i32) -> String {
    let known: &[(i32, &str)] = &[
        (1, "SIGHUP"),
        (2, "SIGINT"),
        (3, "SIGQUIT"),
        (4, "SIGILL"),
        (6, "SIGABRT"),
        (8, "SIGFPE"),
        (9, "SIGKILL"),
        (11, "SIGSEGV"),
        (13, "SIGPIPE"),
        (14, "SIGALRM"),
        (15, "SIGTERM"),
    ];
    known
        .iter()
        .find(|(number, _)| *number == signal)
        .map_or_else(|| format!("SIG{signal}"), |(_, name)| (*name).to_string())
}
