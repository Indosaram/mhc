use std::collections::HashMap;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioMode {
    Pipe,
    Inherit,
    Ignore,
}

impl StdioMode {
    fn to_stdio(self) -> Stdio {
        match self {
            StdioMode::Pipe => Stdio::piped(),
            StdioMode::Inherit => Stdio::inherit(),
            StdioMode::Ignore => Stdio::null(),
        }
    }
}

type AbortListener = Box<dyn FnOnce() + Send + 'static>;

#[derive(Default)]
struct AbortState {
    aborted: bool,
    listeners: Vec<(u64, AbortListener)>,
    next_id: u64,
}

/// Rust counterpart of the platform `AbortSignal` the pinned shim forwards to `spawn`.
#[derive(Clone, Default)]
pub struct AbortSignal {
    state: Arc<Mutex<AbortState>>,
}

impl std::fmt::Debug for AbortSignal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("AbortSignal").field("aborted", &self.aborted()).finish()
    }
}

impl PartialEq for AbortSignal {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl Eq for AbortSignal {}

impl AbortSignal {
    fn lock(&self) -> MutexGuard<'_, AbortState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn aborted(&self) -> bool {
        self.lock().aborted
    }

    /// Register a one-shot listener; an already-aborted signal returns `None`.
    pub fn add_abort_listener(
        &self,
        listener: impl FnOnce() + Send + 'static,
    ) -> Option<AbortRegistration> {
        let mut state = self.lock();
        if state.aborted {
            return None;
        }
        let id = state.next_id;
        state.next_id += 1;
        state.listeners.push((id, Box::new(listener)));
        Some(AbortRegistration { signal: self.clone(), id })
    }

    fn remove_abort_listener(&self, id: u64) {
        self.lock().listeners.retain(|(listener_id, _)| *listener_id != id);
    }

    pub fn abort(&self) {
        let listeners = {
            let mut state = self.lock();
            if state.aborted {
                return;
            }
            state.aborted = true;
            std::mem::take(&mut state.listeners)
        };
        for (_, listener) in listeners {
            listener();
        }
    }
}

/// Dropping the registration unsubscribes exactly this listener.
pub struct AbortRegistration {
    signal: AbortSignal,
    id: u64,
}

impl Drop for AbortRegistration {
    fn drop(&mut self) {
        self.signal.remove_abort_listener(self.id);
    }
}

#[derive(Clone, Debug, Default)]
pub struct AbortController {
    signal: AbortSignal,
}

impl AbortController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn signal(&self) -> AbortSignal {
        self.signal.clone()
    }

    pub fn abort(&self) {
        self.signal.abort();
    }
}

#[cfg(target_os = "linux")]
mod pidfd {
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::sync::Arc;

    #[derive(Clone, Debug)]
    pub struct ProcessHandle {
        pidfd: Arc<OwnedFd>,
    }

    impl ProcessHandle {
        pub(crate) fn new(pidfd: Arc<OwnedFd>) -> Self {
            Self { pidfd }
        }

        pub fn terminate(&self) -> io::Result<()> {
            send(&self.pidfd, libc::SIGTERM)
        }

        pub fn kill(&self) -> io::Result<()> {
            send(&self.pidfd, libc::SIGKILL)
        }
    }

    pub fn open(pid: u32) -> io::Result<Arc<OwnedFd>> {
        // SAFETY: pidfd_open takes scalar arguments and returns a fresh descriptor or -1.
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0_u32) };
        if descriptor == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful syscall returned a descriptor with no other Rust owner.
        Ok(Arc::new(unsafe { OwnedFd::from_raw_fd(descriptor as RawFd) }))
    }

    pub fn terminate(pidfd: &OwnedFd) -> io::Result<()> {
        send(pidfd, libc::SIGTERM)
    }

    fn send(pidfd: &OwnedFd, signal: i32) -> io::Result<()> {
        // SAFETY: the borrowed OwnedFd remains open across the syscall; null siginfo is allowed.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::c_void>(),
                0_u32,
            )
        };
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
pub use pidfd::ProcessHandle;

/// Spawn options; unset stdio defaults to stdin=ignore, stdout=pipe, stderr=inherit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnOptions {
    pub cwd: Option<PathBuf>,
    /// Replaces the whole child environment when set.
    pub env: Option<HashMap<String, String>>,
    pub stdin: Option<StdioMode>,
    pub stdout: Option<StdioMode>,
    pub stderr: Option<StdioMode>,
    pub stdio: Option<[StdioMode; 3]>,
    pub detached: bool,
    pub signal: Option<AbortSignal>,
}

impl SpawnOptions {
    fn resolved_stdio(&self) -> [StdioMode; 3] {
        self.stdio.unwrap_or([
            self.stdin.unwrap_or(StdioMode::Ignore),
            self.stdout.unwrap_or(StdioMode::Pipe),
            self.stderr.unwrap_or(StdioMode::Inherit),
        ])
    }
}

fn build_command(cmd: &[String], options: &SpawnOptions) -> io::Result<Command> {
    let (bin, args) = cmd
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "spawn requires a command"))?;
    let mut command = Command::new(bin);
    command.args(args);
    if let Some(cwd) = &options.cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = &options.env {
        command.env_clear().envs(env);
    }
    let [stdin, stdout, stderr] = options.resolved_stdio();
    command
        .stdin(stdin.to_stdio())
        .stdout(stdout.to_stdio())
        .stderr(stderr.to_stdio());
    #[cfg(unix)]
    if options.detached {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(command)
}

/// A running child. Streams are `None` unless piped.
pub struct SpawnedProcess {
    child: Child,
    pid: u32,
    exit_code: Option<i32>,
    _cancellation: Option<AbortRegistration>,
    #[cfg(target_os = "linux")]
    pidfd: Option<Arc<OwnedFd>>,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
}

fn code_of(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

impl SpawnedProcess {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    #[cfg(target_os = "linux")]
    pub fn handle(&mut self) -> io::Result<ProcessHandle> {
        let pidfd = match &self.pidfd {
            Some(pidfd) => Arc::clone(pidfd),
            None => {
                if self.exit_code.is_some() {
                    return Err(io::Error::new(io::ErrorKind::NotFound, "child has already been reaped"));
                }
                let pidfd = pidfd::open(self.pid)?;
                self.pidfd = Some(Arc::clone(&pidfd));
                pidfd
            }
        };
        Ok(ProcessHandle::new(pidfd))
    }

    /// Wait for exit; the exit code is cached for [`Self::exit_code`].
    pub fn exited(&mut self) -> io::Result<i32> {
        if let Some(code) = self.exit_code {
            return Ok(code);
        }
        drop(self.stdin.take());
        let code = code_of(self.child.wait()?);
        self.exit_code = Some(code);
        self._cancellation = None;
        Ok(code)
    }

    /// Drain stdout and stderr fully, then wait (avoids pipe deadlock).
    pub fn wait_with_output(mut self) -> io::Result<(i32, String, String)> {
        drop(self.stdin.take());
        let stdout = self.stdout.take();
        let stderr = self.stderr.take();
        let err_reader = std::thread::spawn(move || read_all(stderr));
        let out = read_all(stdout);
        let err = err_reader.join().unwrap_or_default();
        let code = match self.exit_code {
            Some(code) => code,
            None => code_of(self.child.wait()?),
        };
        self.exit_code = Some(code);
        self._cancellation = None;
        Ok((code, out, err))
    }

    pub fn kill(&mut self) {
        if self.exit_code.is_none() {
            let _ = self.child.kill();
        }
    }
}

fn read_all(stream: Option<impl Read>) -> String {
    let mut buffer = Vec::new();
    if let Some(mut stream) = stream {
        let _ = stream.read_to_end(&mut buffer);
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

pub fn spawn(cmd: &[String], options: &SpawnOptions) -> io::Result<SpawnedProcess> {
    #[cfg(not(target_os = "linux"))]
    if options.signal.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "SpawnOptions.signal cancellation is not supported on this platform yet",
        ));
    }
    let mut child = build_command(cmd, options)?.spawn()?;
    let pid = child.id();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    #[cfg(target_os = "linux")]
    let (cancellation, pidfd) = match &options.signal {
        None => (None, None),
        Some(signal) => {
            let pidfd = match pidfd::open(pid) {
                Ok(pidfd) => pidfd,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            };
            let listener_pidfd = Arc::clone(&pidfd);
            match signal.add_abort_listener(move || {
                let _ = pidfd::terminate(&listener_pidfd);
            }) {
                Some(registration) => (Some(registration), Some(pidfd)),
                None => {
                    let _ = pidfd::terminate(&pidfd);
                    (None, Some(pidfd))
                }
            }
        }
    };
    #[cfg(not(target_os = "linux"))]
    let cancellation = None;
    Ok(SpawnedProcess {
        child,
        pid,
        exit_code: None,
        _cancellation: cancellation,
        #[cfg(target_os = "linux")]
        pidfd,
        stdin,
        stdout,
        stderr,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSyncResult {
    pub exit_code: i32,
    pub stdout: Option<Vec<u8>>,
    pub stderr: Option<Vec<u8>>,
    pub success: bool,
    pub pid: i64,
}

pub fn spawn_sync(cmd: &[String], options: &SpawnOptions) -> io::Result<SpawnSyncResult> {
    let [_, stdout_mode, stderr_mode] = options.resolved_stdio();
    let child = build_command(cmd, options)?.spawn()?;
    let pid = i64::from(child.id());
    let output = child.wait_with_output()?;
    let exit_code = code_of(output.status);
    Ok(SpawnSyncResult {
        exit_code,
        stdout: (stdout_mode == StdioMode::Pipe).then_some(output.stdout),
        stderr: (stderr_mode == StdioMode::Pipe).then_some(output.stderr),
        success: exit_code == 0,
        pid,
    })
}
