//! Process launches behind the execution barrier, for code running in a tool-child worker thread (senpi
//! `tool-child/gated-child-process.ts`; senpi#3566, plan todo 4; the per-API table in DESIGN PART 1).
//!
//! mhc has no `node:child_process` module to swap, so the gate is a launch API of its own that wraps
//! `std::process::Command`: every spawn first claims a slot through the worker's `WorkerLaunchClient` (blocking the
//! worker until the ack), stores the invoke tick, and only then starts the command. Synchronous and `exec*` launches
//! stay in the child's own (ledgered) group. `spawn` without `detached` and with `detached: true` leads its own group
//! through the POSIX go-line shim, so the handle is on record before the program runs; `detached: true` is ledgered as
//! detached and never swept. On Windows every launch is contained by the job and reports the command it started;
//! `detached: true` starts the command outside the job (`CreateProcessW` with breakaway) in this process, so upstream's
//! `launch-shim` re-entry process (which only exists because Node cannot call kernel32) is not needed.
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use crate::go_shim::{ErrnoException, Shell, enoent_error, shim_target};
use crate::launch_barrier::{ExitOutcome, StdioKind};
use crate::launch_env_names::{ProcessEnv, process_env, without_internal_env};
use crate::protocol::{LaunchApi, LaunchContainment, LaunchLifetime, LaunchOrigin};
use crate::windows_breakaway::{BreakawayError, load_kernel32_process, start_breakaway};
use crate::windows_command_line::{quote_windows_argument, windows_command_line};
use crate::worker_launch::{
    WorkerLaunchClient, WorkerLaunchError, WorkerLaunchOutcome, WorkerLaunchRequest,
};

pub type SharedClient = Arc<Mutex<WorkerLaunchClient>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a gated launch can fail with: the barrier refused it, the OS refused it, the program does not exist, or (Windows
/// breakaway) kernel32 refused.
#[derive(Debug, thiserror::Error)]
pub enum GatedSpawnError {
    #[error(transparent)]
    Launch(#[from] WorkerLaunchError),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Enoent(Box<ErrnoException>),
    #[error(transparent)]
    Breakaway(#[from] BreakawayError),
    #[error("launches that lead their own group are POSIX-only")]
    PosixOnly,
}

impl From<ErrnoException> for GatedSpawnError {
    fn from(error: ErrnoException) -> Self {
        Self::Enoent(Box::new(error))
    }
}

/// The `SpawnOptions` a launch accepts.
#[derive(Debug, Clone)]
pub struct SpawnOptions {
    pub cwd: Option<PathBuf>,
    /// The command's environment; the current process environment when absent.
    pub env: Option<HashMap<String, String>>,
    pub stdio: [StdioKind; 3],
    /// `None` leads its own group, owned; `Some(true)` leads one, detached; `Some(false)` stays in the child's group.
    pub detached: Option<bool>,
    pub shell: Option<Shell>,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            cwd: None,
            env: None,
            stdio: [StdioKind::Pipe; 3],
            detached: None,
            shell: None,
        }
    }
}

fn stdio_of(kind: StdioKind) -> Stdio {
    match kind {
        StdioKind::Pipe => Stdio::piped(),
        StdioKind::Ignore => Stdio::null(),
        StdioKind::Inherit => Stdio::inherit(),
    }
}

/// A launched command: the handle a caller gets back. Its completion is reported to the gate exactly once.
pub struct GatedSpawn {
    pub launch_id: String,
    /// The pid; for an own-group launch also the pid and process group of the command once the shim execs it.
    pub pid: u32,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    exited: Mutex<mpsc::Receiver<ExitOutcome>>,
    outcome: OnceLock<ExitOutcome>,
}

impl GatedSpawn {
    /// Blocks until the command exited (the `exit` event).
    pub fn wait(&self) -> ExitOutcome {
        self.outcome
            .get_or_init(|| {
                lock(&self.exited).recv().unwrap_or(ExitOutcome {
                    exit_code: None,
                    signal: None,
                    enoent: false,
                })
            })
            .clone()
    }

    /// The `'error'` event upstream emits when the go-line shim reports the program missing: `spawn <command> ENOENT`
    /// (`enoentError(command, args)`). Waits for the exit like [`GatedSpawn::wait`]; `None` when the program ran.
    pub fn spawn_error(&self, command: &str, args: &[String]) -> Option<ErrnoException> {
        self.wait().enoent.then(|| enoent_error(command, args))
    }
}

#[cfg(unix)]
fn signal_name(status: &ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    let name = match status.signal()? {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGABRT => "SIGABRT",
        libc::SIGKILL => "SIGKILL",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGTERM => "SIGTERM",
        other => return Some(format!("SIG{other}")),
    };
    Some(name.to_string())
}

#[cfg(not(unix))]
fn signal_name(_status: &ExitStatus) -> Option<String> {
    None
}

/// The shim's control FIFO (the same technique as `launch_barrier`: std's `Command` cannot hand a child an extra pipe
/// descriptor without `pre_exec`, so the shim opens this FIFO as its control descriptor).
#[cfg(unix)]
struct Control {
    path: PathBuf,
    file: std::fs::File,
}

#[cfg(unix)]
static CONTROL_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(unix)]
impl Control {
    fn create() -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = std::env::temp_dir().join(format!(
            "maho-gcp-{}-{}",
            std::process::id(),
            CONTROL_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        crate::go_shim::make_fifo(&path)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&path);
            })?;
        Ok(Self { path, file })
    }

    fn write_go(&mut self) {
        use std::io::Write;
        // Three bytes into an empty pipe never block; a failure means the shim is already gone.
        let _ = self.file.write_all(b"go\n");
    }

    /// What the shim wrote back: the program was not found.
    fn reported_enoent(&mut self) -> bool {
        use std::io::Read;
        let mut reply = Vec::new();
        let _ = self.file.read_to_end(&mut reply);
        String::from_utf8_lossy(&reply)
            .lines()
            .any(|line| line == "ENOENT")
    }
}

#[cfg(unix)]
impl Drop for Control {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(not(unix))]
struct Control;

#[cfg(not(unix))]
impl Control {
    fn reported_enoent(&mut self) -> bool {
        false
    }
}

fn report_done(
    client: &SharedClient,
    launch_id: &str,
    exit_code: Option<i32>,
    signal: Option<String>,
) {
    lock(client).done(launch_id, WorkerLaunchOutcome { exit_code, signal });
}

/// Reports the launch done exactly once, on exit, from a thread that reaps the child.
fn track_exit(
    client: &SharedClient,
    launch_id: &str,
    mut child: Child,
    control: Option<Arc<Mutex<Control>>>,
) -> io::Result<GatedSpawn> {
    let pid = child.id();
    let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let (publish, exited) = mpsc::channel();
    let (client, id) = (Arc::clone(client), launch_id.to_string());
    std::thread::Builder::new()
        .name("gated-reaper".to_string())
        .spawn(move || {
            let status = child.wait().ok();
            let enoent = control
                .as_ref()
                .is_some_and(|control| lock(control).reported_enoent());
            let (code, signal) = match &status {
                Some(status) => (status.code(), signal_name(status)),
                None => (None, None),
            };
            // An ENOENT report is an `'error'` upstream, which completes the launch with exit code 1.
            report_done(
                &client,
                &id,
                if enoent { Some(1) } else { code },
                signal.clone(),
            );
            let _ = publish.send(ExitOutcome {
                exit_code: code,
                signal,
                enoent,
            });
        })?;
    Ok(GatedSpawn {
        launch_id: launch_id.to_string(),
        pid,
        stdin,
        stdout,
        stderr,
        exited: Mutex::new(exited),
        outcome: OnceLock::new(),
    })
}

fn command_for(program: &str, args: &[String], options: &SpawnOptions) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(cwd) = &options.cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = &options.env {
        command.env_clear().envs(env);
    }
    command
        .stdin(stdio_of(options.stdio[0]))
        .stdout(stdio_of(options.stdio[1]))
        .stderr(stdio_of(options.stdio[2]));
    command
}

fn request(
    api: LaunchApi,
    containment: LaunchContainment,
    lifetime: LaunchLifetime,
) -> WorkerLaunchRequest {
    WorkerLaunchRequest {
        origin: LaunchOrigin::Kernel,
        api,
        containment,
        lifetime,
        pid: None,
        pgid: None,
    }
}

/// A call that leads its own group: the shim holds the command until the ack and the invoke tick are recorded.
#[cfg(unix)]
pub(crate) fn own_group_spawn(
    client: &SharedClient,
    api: LaunchApi,
    program: &str,
    args: &[String],
    options: &SpawnOptions,
    lifetime: LaunchLifetime,
) -> Result<GatedSpawn, GatedSpawnError> {
    use crate::go_shim::{control_fd, go_shim_script};
    use std::os::unix::process::CommandExt;
    let fd = control_fd(&options.stdio);
    let control = Control::create()?;
    let target = shim_target(program, args, options.shell.as_ref());
    let quoted = format!(
        "'{}'",
        control.path.to_string_lossy().replace('\'', "'\\''")
    );
    let script = format!("exec {fd}<>{quoted}; {}", go_shim_script(fd));
    let mut shim_args = vec!["-c".to_string(), script, target.program];
    shim_args.extend(target.args);
    let mut command = command_for("/bin/sh", &shim_args, options);
    command.process_group(0);
    let mut shim = command.spawn()?;
    let pid = shim.id();
    let launch_id = match lock(client).acquire(WorkerLaunchRequest {
        origin: LaunchOrigin::Kernel,
        api,
        containment: LaunchContainment::OwnGroup,
        lifetime,
        pid: Some(pid),
        pgid: Some(pid),
    }) {
        Ok(id) => id,
        Err(error) => {
            let _ = shim.kill();
            let _ = shim.wait();
            return Err(error.into());
        }
    };
    let control = Arc::new(Mutex::new(control));
    let spawned = track_exit(client, &launch_id, shim, Some(Arc::clone(&control)))?;
    lock(client).invoke(&launch_id)?;
    lock(&control).write_go();
    Ok(spawned)
}

#[cfg(not(unix))]
pub(crate) fn own_group_spawn(
    _client: &SharedClient,
    _api: LaunchApi,
    _program: &str,
    _args: &[String],
    _options: &SpawnOptions,
    _lifetime: LaunchLifetime,
) -> Result<GatedSpawn, GatedSpawnError> {
    Err(GatedSpawnError::PosixOnly)
}

/// A call that stays in the child's own group (or, on Windows, the job): ack first, then the invoke tick, then the call.
pub(crate) fn inherited_spawn(
    client: &SharedClient,
    api: LaunchApi,
    containment: LaunchContainment,
    program: &str,
    args: &[String],
    options: &SpawnOptions,
) -> Result<GatedSpawn, GatedSpawnError> {
    let launch_id = {
        let mut guard = lock(client);
        let id = guard.acquire(request(api, containment, LaunchLifetime::Owned))?;
        guard.invoke(&id)?;
        id
    };
    let target = shim_target(program, args, options.shell.as_ref());
    match command_for(&target.program, &target.args, options).spawn() {
        Ok(child) => {
            if containment == LaunchContainment::Job {
                lock(client).started(&launch_id, child.id(), false);
            }
            Ok(track_exit(client, &launch_id, child, None)?)
        }
        Err(error) => {
            report_done(client, &launch_id, Some(1), None);
            Err(error.into())
        }
    }
}

/// The result of a synchronous launch.
#[derive(Debug, Clone)]
pub struct SyncOutput {
    pub pid: u32,
    pub status: Option<i32>,
    pub signal: Option<String>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// A synchronous call: blocks on the ack, records the invoke tick, runs the command to completion, reports it done.
pub(crate) fn run_inherited_sync(
    client: &SharedClient,
    api: LaunchApi,
    containment: LaunchContainment,
    program: &str,
    args: &[String],
    options: &SpawnOptions,
) -> Result<SyncOutput, GatedSpawnError> {
    let launch_id = {
        let mut guard = lock(client);
        let id = guard.acquire(request(api, containment, LaunchLifetime::Owned))?;
        guard.invoke(&id)?;
        id
    };
    let target = shim_target(program, args, options.shell.as_ref());
    let mut command = command_for(&target.program, &target.args, options);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let spawned = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            report_done(client, &launch_id, Some(1), None);
            return Err(error.into());
        }
    };
    let pid = spawned.id();
    if containment == LaunchContainment::Job {
        lock(client).started(&launch_id, pid, false);
    }
    match spawned.wait_with_output() {
        Ok(output) => {
            let signal = signal_name(&output.status);
            report_done(client, &launch_id, output.status.code(), signal.clone());
            Ok(SyncOutput {
                pid,
                status: output.status.code(),
                signal,
                stdout: output.stdout,
                stderr: output.stderr,
            })
        }
        Err(error) => {
            report_done(client, &launch_id, Some(1), None);
            Err(error.into())
        }
    }
}

/// `{ stdout, stderr }` of a command that exited 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
}

/// A failed `exec`: like Node, the error carries what the command wrote and its exit code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Command failed: {command}\n{stderr}")]
pub struct ExecError {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
    pub signal: Option<String>,
}

/// `node:child_process` as a gated module: the same launch functions, each passing the barrier.
#[derive(Clone)]
pub struct GatedChildProcess {
    client: SharedClient,
    win32: bool,
}

/// `createGatedChildProcess(module, client, platform)`: `win32` selects the Windows containment (the job) and the
/// `started` report; it is injectable so the Windows mode is testable on every OS.
pub fn create_gated_child_process(client: SharedClient, win32: bool) -> GatedChildProcess {
    GatedChildProcess { client, win32 }
}

impl GatedChildProcess {
    pub fn client(&self) -> &SharedClient {
        &self.client
    }

    fn inherited(&self) -> LaunchContainment {
        if self.win32 {
            LaunchContainment::Job
        } else {
            LaunchContainment::Inherited
        }
    }

    /// `spawn`.
    pub fn spawn(
        &self,
        command: &str,
        args: &[String],
        options: &SpawnOptions,
    ) -> Result<GatedSpawn, GatedSpawnError> {
        if self.win32 && options.detached == Some(true) {
            return self.breakaway_spawn(command, args, options);
        }
        if !self.win32 && options.detached != Some(false) {
            let lifetime = if options.detached == Some(true) {
                LaunchLifetime::Detached
            } else {
                LaunchLifetime::Owned
            };
            return own_group_spawn(
                &self.client,
                LaunchApi::CpSpawn,
                command,
                args,
                options,
                lifetime,
            );
        }
        inherited_spawn(
            &self.client,
            LaunchApi::CpSpawn,
            self.inherited(),
            command,
            args,
            options,
        )
    }

    /// `spawnSync`.
    pub fn spawn_sync(
        &self,
        command: &str,
        args: &[String],
        options: &SpawnOptions,
    ) -> Result<SyncOutput, GatedSpawnError> {
        run_inherited_sync(
            &self.client,
            LaunchApi::CpSpawnSync,
            self.inherited(),
            command,
            args,
            options,
        )
    }

    /// `execFileSync`: stdout of a command that exited 0.
    pub fn exec_file_sync(
        &self,
        file: &str,
        args: &[String],
        options: &SpawnOptions,
    ) -> Result<Result<Vec<u8>, ExecError>, GatedSpawnError> {
        let output = run_inherited_sync(
            &self.client,
            LaunchApi::CpExecFileSync,
            self.inherited(),
            file,
            args,
            options,
        )?;
        Ok(exec_result(file, output).map(|ok| ok.stdout.into_bytes()))
    }

    /// `execSync`: the command line runs through the platform shell.
    pub fn exec_sync(
        &self,
        line: &str,
        options: &SpawnOptions,
    ) -> Result<Result<Vec<u8>, ExecError>, GatedSpawnError> {
        let (shell, args) = shell_line(self.win32, line);
        let output = run_inherited_sync(
            &self.client,
            LaunchApi::CpExecFileSync,
            self.inherited(),
            &shell,
            &args,
            options,
        )?;
        Ok(exec_result(line, output).map(|ok| ok.stdout.into_bytes()))
    }

    /// The promisified `exec`: runs the gated launch to completion and resolves `{ stdout, stderr }`, or fails with
    /// them attached, like `promisify(exec)`.
    pub fn exec(
        &self,
        line: &str,
        options: &SpawnOptions,
    ) -> Result<Result<ExecOutput, ExecError>, GatedSpawnError> {
        let (shell, args) = shell_line(self.win32, line);
        self.exec_async(line, &shell, &args, options)
    }

    /// The promisified `execFile`.
    pub fn exec_file(
        &self,
        file: &str,
        args: &[String],
        options: &SpawnOptions,
    ) -> Result<Result<ExecOutput, ExecError>, GatedSpawnError> {
        self.exec_async(file, file, args, options)
    }

    fn exec_async(
        &self,
        label: &str,
        program: &str,
        args: &[String],
        options: &SpawnOptions,
    ) -> Result<Result<ExecOutput, ExecError>, GatedSpawnError> {
        let mut options = options.clone();
        options.stdio = [StdioKind::Ignore, StdioKind::Pipe, StdioKind::Pipe];
        let mut spawned = inherited_spawn(
            &self.client,
            LaunchApi::CpSpawn,
            self.inherited(),
            program,
            args,
            &options,
        )?;
        let (stdout, stderr) = (spawned.stdout.take(), spawned.stderr.take());
        // Both pipes are drained concurrently so neither can fill and block the command.
        let err_reader = std::thread::spawn(move || read_all(stderr));
        let out = read_all(stdout);
        let err = err_reader.join().unwrap_or_default();
        let outcome = spawned.wait();
        let output = SyncOutput {
            pid: spawned.pid,
            status: outcome.exit_code,
            signal: outcome.signal,
            stdout: out,
            stderr: err,
        };
        Ok(exec_result(label, output))
    }

    /// Windows `detached: true`: the command must outlive the tool child, so it starts outside the child's Job Object.
    fn breakaway_spawn(
        &self,
        command: &str,
        args: &[String],
        options: &SpawnOptions,
    ) -> Result<GatedSpawn, GatedSpawnError> {
        let launch_id = {
            let mut guard = lock(&self.client);
            let id = guard.acquire(request(
                LaunchApi::CpSpawn,
                LaunchContainment::Breakaway,
                LaunchLifetime::Detached,
            ))?;
            guard.invoke(&id)?;
            id
        };
        let source = options.env.clone().unwrap_or_else(process_env);
        let env = without_internal_env(&source);
        let line = breakaway_command_line(command, args, options.shell.as_ref(), &source);
        let started = load_kernel32_process()
            .ok_or_else(|| BreakawayError::new("LoadKernel32", 0))
            .and_then(|kernel| start_breakaway(kernel, &line, &env));
        let process = match started {
            Ok(process) => process,
            Err(error) => {
                report_done(&self.client, &launch_id, Some(1), None);
                return Err(error.into());
            }
        };
        let pid = process.pid;
        lock(&self.client).started(&launch_id, pid, true);
        let (publish, exited) = mpsc::channel();
        let (client, id) = (Arc::clone(&self.client), launch_id.clone());
        std::thread::Builder::new()
            .name("gated-breakaway".to_string())
            .spawn(move || {
                let code = process
                    .wait()
                    .ok()
                    .and_then(|code| i32::try_from(code).ok());
                report_done(&client, &id, code.or(Some(1)), None);
                let _ = publish.send(ExitOutcome {
                    exit_code: code,
                    signal: None,
                    enoent: false,
                });
            })?;
        Ok(GatedSpawn {
            launch_id,
            pid,
            stdin: None,
            stdout: None,
            stderr: None,
            exited: Mutex::new(exited),
            outcome: OnceLock::new(),
        })
    }
}

fn shell_line(win32: bool, line: &str) -> (String, Vec<String>) {
    if win32 {
        (
            "cmd.exe".to_string(),
            ["/d", "/s", "/c", line].map(String::from).to_vec(),
        )
    } else {
        (
            "/bin/sh".to_string(),
            ["-c", line].map(String::from).to_vec(),
        )
    }
}

fn read_all<R: io::Read>(reader: Option<R>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(mut reader) = reader {
        let _ = reader.read_to_end(&mut bytes);
    }
    bytes
}

fn exec_result(command: &str, output: SyncOutput) -> Result<ExecOutput, ExecError> {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if output.status == Some(0) {
        Ok(ExecOutput { stdout, stderr })
    } else {
        Err(ExecError {
            command: command.to_string(),
            stdout,
            stderr,
            code: output.status,
            signal: output.signal,
        })
    }
}

/// The command line the breakaway launch starts, by Node's own rules for `shell` on Windows: a shell named `cmd` or
/// `cmd.exe` gets `/d /s /c "<command>"` with the command passed through verbatim (cmd.exe does not understand the C
/// runtime's `\"` escaping); any other shell gets `-c <command>` with normal quoting. `shell: true` means `%ComSpec%`,
/// else cmd.exe.
pub fn breakaway_command_line(
    command: &str,
    args: &[String],
    shell: Option<&Shell>,
    env: &ProcessEnv,
) -> String {
    let program = match shell {
        None | Some(Shell::Enabled(false)) => {
            let mut argv = vec![command.to_string()];
            argv.extend(args.iter().cloned());
            return windows_command_line(&argv);
        }
        Some(Shell::Path(path)) => path.clone(),
        Some(Shell::Enabled(true)) => env
            .get("ComSpec")
            .cloned()
            .unwrap_or_else(|| "cmd.exe".to_string()),
    };
    let line = std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let name = program
        .rsplit('\\')
        .next()
        .unwrap_or(&program)
        .to_lowercase();
    if name == "cmd" || name == "cmd.exe" {
        return format!("{} /d /s /c \"{line}\"", quote_windows_argument(&program));
    }
    windows_command_line(&[program, "-c".to_string(), line])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::barrier_clock::BarrierClock;
    #[cfg(unix)]
    use crate::worker_launch::{WorkerLaunchClientOptions, WorkerLaunchMessage};

    #[cfg(unix)]
    fn recording() -> (
        GatedChildProcess,
        GatedChildProcess,
        Arc<Mutex<Vec<WorkerLaunchMessage>>>,
    ) {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&messages);
        let client = Arc::new(Mutex::new(WorkerLaunchClient::new(
            WorkerLaunchClientOptions {
                clock: BarrierClock::create(4).expect("clock"),
                post: Box::new(move |message| lock(&sink).push(message)),
                job_id: Box::new(|| "job-1".to_string()),
                skip_ack_wait: true,
                ack_timeout_ms: Some(5_000),
                worker_id: None,
            },
        )));
        (
            create_gated_child_process(Arc::clone(&client), true),
            create_gated_child_process(client, false),
            messages,
        )
    }

    #[cfg(unix)]
    fn launched(messages: &Arc<Mutex<Vec<WorkerLaunchMessage>>>) -> Option<WorkerLaunchRequest> {
        lock(messages).iter().find_map(|message| match message {
            WorkerLaunchMessage::Launch { request, .. } => Some(WorkerLaunchRequest {
                origin: request.origin,
                api: request.api,
                containment: request.containment,
                lifetime: request.lifetime,
                pid: request.pid,
                pgid: request.pgid,
            }),
            _ => None,
        })
    }

    #[cfg(unix)]
    #[test]
    fn records_job_containment_and_reports_the_started_pid_for_spawn_sync_in_win32_mode() {
        let (win32, _, messages) = recording();
        let result = win32
            .spawn_sync("true", &[], &SpawnOptions::default())
            .expect("spawn_sync");
        assert_eq!(
            launched(&messages).map(|request| request.containment),
            Some(LaunchContainment::Job)
        );
        let started: Vec<_> = lock(&messages)
            .iter()
            .filter_map(|message| match message {
                WorkerLaunchMessage::Started { pid, breakaway, .. } => Some((*pid, *breakaway)),
                _ => None,
            })
            .collect();
        assert_eq!(started, vec![(result.pid, false)]);
    }

    #[cfg(unix)]
    #[test]
    fn keeps_posix_launches_inherited_with_no_started_report() {
        let (_, posix, messages) = recording();
        posix
            .spawn_sync("true", &[], &SpawnOptions::default())
            .expect("spawn_sync");
        assert_eq!(
            launched(&messages).map(|request| request.containment),
            Some(LaunchContainment::Inherited)
        );
        assert!(
            !lock(&messages)
                .iter()
                .any(|message| matches!(message, WorkerLaunchMessage::Started { .. }))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_spawn_leads_its_own_group_and_is_reported_done_once() {
        let (_, posix, messages) = recording();
        let options = SpawnOptions {
            stdio: [StdioKind::Ignore; 3],
            ..SpawnOptions::default()
        };
        let child = posix.spawn("true", &[], &options).expect("spawn");
        assert_eq!(child.wait().exit_code, Some(0));
        let request = launched(&messages).expect("launch");
        assert_eq!(
            (request.api, request.containment, request.lifetime),
            (
                LaunchApi::CpSpawn,
                LaunchContainment::OwnGroup,
                LaunchLifetime::Owned
            )
        );
        assert_eq!(request.pid, request.pgid);
        // The reaper reports after it published the exit; `wait` returning means `done` was posted first.
        let dones = lock(&messages)
            .iter()
            .filter(|message| matches!(message, WorkerLaunchMessage::Done { .. }))
            .count();
        assert_eq!(dones, 1);
    }

    #[cfg(unix)]
    #[test]
    fn detached_is_ledgered_as_detached() {
        let (_, posix, messages) = recording();
        let options = SpawnOptions {
            stdio: [StdioKind::Ignore; 3],
            detached: Some(true),
            ..SpawnOptions::default()
        };
        posix.spawn("true", &[], &options).expect("spawn").wait();
        assert_eq!(
            launched(&messages).map(|request| request.lifetime),
            Some(LaunchLifetime::Detached)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_program_is_reported_as_enoent() {
        let (_, posix, _) = recording();
        let options = SpawnOptions {
            stdio: [StdioKind::Ignore; 3],
            ..SpawnOptions::default()
        };
        let child = posix
            .spawn("definitely-not-a-program-xyz", &[], &options)
            .expect("spawn");
        assert!(child.wait().enoent);
    }

    #[cfg(unix)]
    #[test]
    fn exec_resolves_stdout_and_stderr_and_a_failure_carries_them() {
        let (_, posix, messages) = recording();
        let ok = posix
            .exec("echo hi", &SpawnOptions::default())
            .expect("exec")
            .expect("exit 0");
        assert_eq!(
            ok,
            ExecOutput {
                stdout: "hi\n".to_string(),
                stderr: String::new()
            }
        );
        assert_eq!(
            launched(&messages).map(|request| request.api),
            Some(LaunchApi::CpSpawn)
        );
        let failed = posix
            .exec("echo out; echo err >&2; exit 3", &SpawnOptions::default())
            .expect("exec")
            .expect_err("exit 3");
        assert_eq!(
            (failed.stdout.as_str(), failed.stderr.as_str(), failed.code),
            ("out\n", "err\n", Some(3))
        );
    }

    #[test]
    fn builds_a_detached_shell_launchs_command_line_by_the_shells_own_convention() {
        let env = |comspec: Option<&str>| -> ProcessEnv {
            comspec
                .map(|value| ("ComSpec".to_string(), value.to_string()))
                .into_iter()
                .collect()
        };
        let hi = ["hi".to_string()];
        let cases: [(Shell, ProcessEnv, &str); 5] = [
            (
                Shell::Enabled(true),
                env(None),
                "cmd.exe /d /s /c \"echo hi\"",
            ),
            (
                Shell::Enabled(true),
                env(Some("C:\\Windows\\System32\\cmd.exe")),
                "C:\\Windows\\System32\\cmd.exe /d /s /c \"echo hi\"",
            ),
            (
                Shell::Path("cmd".to_string()),
                env(None),
                "cmd /d /s /c \"echo hi\"",
            ),
            (
                Shell::Path("powershell.exe".to_string()),
                env(None),
                "powershell.exe -c \"echo hi\"",
            ),
            (
                Shell::Path("C:\\Program Files\\Git\\bin\\bash.exe".to_string()),
                env(None),
                "\"C:\\Program Files\\Git\\bin\\bash.exe\" -c \"echo hi\"",
            ),
        ];
        for (shell, env, expected) in cases {
            assert_eq!(
                breakaway_command_line("echo", &hi, Some(&shell), &env),
                expected
            );
        }
    }
}
