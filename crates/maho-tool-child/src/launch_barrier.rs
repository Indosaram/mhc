//! Ack-gated launch paths for the tool child's own (main-thread) launches (senpi#3566, plan todo 4).
//!
//! `launch_own_group` (POSIX; `lifetime: Detached` for an explicitly detached launch) starts the go-line shim
//! (`go_shim.rs`), which leads a new process group and blocks on a go-line read from a control descriptor (it exits 127 if
//! that closes first, e.g. when the child died); stdin stays the caller's. Its pid and group are sent as the containment
//! handle; only after the server acknowledged does the gate store the invoke tick and write the go-line, and only then
//! does the shim `exec` the real command, keeping pid and group. `gate_inherited` is the same barrier for a call that
//! stays in the child's own group: wait for the ack, record the invoke tick, then call. Kernel-worker launches and
//! Windows breakaway launches live in their own modules.
//!
//! The control descriptor: Node hands the shim an extra `"pipe"` stdio entry. `std::process::Command` can place only
//! descriptors 0-2 and this crate forbids the `unsafe` a `pre_exec` `dup2` needs, so the control channel is a FIFO the
//! parent opens read-write first and the shim shell opens as descriptor `fd` (`exec fd<>fifo`) before running the
//! unchanged go-line script. The FIFO is a duplex pipe like Node's: the parent writes `go\n`, and the shim's `ENOENT`
//! report is read back after the shim exited ([`ExitOutcome::enoent`]).
use std::collections::HashMap;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use tokio::process::{Child, Command};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::watch;

#[cfg(unix)]
use crate::go_shim::{control_fd, go_shim_script, with_control_pipe};
use crate::launch_gate::{LaunchGate, LaunchGateError, LaunchRequest};
use crate::protocol::{LaunchApi, LaunchContainment, LaunchLifetime, LaunchOrigin};

/// `"pipe" | "ignore" | "inherit"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioKind {
    Pipe,
    Ignore,
    Inherit,
}

impl StdioKind {
    #[cfg(unix)]
    fn stdio(self) -> Stdio {
        match self {
            Self::Pipe => Stdio::piped(),
            Self::Ignore => Stdio::null(),
            Self::Inherit => Stdio::inherit(),
        }
    }
}

/// `{ exitCode: number | null, signal: NodeJS.Signals | null }` of the shim, plus what it reported on the control
/// descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitOutcome {
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    /// The shim reported `ENOENT` on the control descriptor: its program could not be found.
    pub enoent: bool,
}

/// The spawn options `launchOwnGroup` accepts (`SpawnOptions` minus `detached`/`stdio`, with the three-entry stdio).
#[derive(Debug, Clone)]
pub struct OwnGroupOptions {
    pub cwd: Option<PathBuf>,
    /// The command's environment; the current process environment when absent.
    pub env: Option<HashMap<String, String>>,
    pub stdio: [StdioKind; 3],
}

impl Default for OwnGroupOptions {
    fn default() -> Self {
        Self {
            cwd: None,
            env: None,
            stdio: [StdioKind::Ignore, StdioKind::Pipe, StdioKind::Pipe],
        }
    }
}

pub struct GatedChild {
    pub launch_id: String,
    /// The shim's pid, which is the pid and process group of the command once it execs.
    pub pid: u32,
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    exited: watch::Receiver<Option<ExitOutcome>>,
}

impl GatedChild {
    /// Resolves when the shim exited (the `exited` promise).
    pub async fn exited(&self) -> ExitOutcome {
        wait_exited(self.exited.clone()).await
    }
}

async fn wait_exited(mut exited: watch::Receiver<Option<ExitOutcome>>) -> ExitOutcome {
    loop {
        if let Some(outcome) = exited.borrow_and_update().clone() {
            return outcome;
        }
        if exited.changed().await.is_err() {
            // The waiter task ended without publishing: the shim cannot be observed any more.
            return ExitOutcome {
                exit_code: None,
                signal: None,
                enoent: false,
            };
        }
    }
}

pub struct OwnGroupLaunch {
    pub gate: Arc<LaunchGate>,
    pub job_id: String,
    pub origin: LaunchOrigin,
    pub api: LaunchApi,
    pub program: String,
    pub args: Vec<String>,
    pub options: Option<OwnGroupOptions>,
    /// Defaults to `Owned`.
    pub lifetime: Option<LaunchLifetime>,
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchBarrierError {
    #[error("launchOwnGroup is POSIX-only; Windows launches are contained by the job")]
    PosixOnly,
    #[error("could not start {0}")]
    CouldNotStart(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Gate(#[from] LaunchGateError),
}

#[cfg(unix)]
static CONTROL_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
fn signal_name(status: &ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    let name = match status.signal()? {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGABRT => "SIGABRT",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGBUS => "SIGBUS",
        libc::SIGSTOP => "SIGSTOP",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGUSR2 => "SIGUSR2",
        other => return Some(format!("SIG{other}")),
    };
    Some(name.to_string())
}

/// The shim's control FIFO, opened read-write by the parent (which never blocks, and keeps the pipe alive).
#[cfg(unix)]
struct Control {
    path: PathBuf,
    file: std::fs::File,
}

#[cfg(unix)]
impl Control {
    fn create(dir: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let path = dir.join(format!(
            "maho-go-{}-{}",
            std::process::id(),
            CONTROL_COUNTER.fetch_add(1, Ordering::Relaxed)
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
        // Three bytes into an empty pipe never block; a failure means the shim is already gone.
        let _ = self.file.write_all(b"go\n");
    }

    /// What the shim wrote back after it exited: the program was not found.
    fn reported_enoent(&mut self) -> bool {
        // Non-blocking: the read ends with `WouldBlock` once the pipe is drained; what was read stays in `reply`.
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

#[cfg(unix)]
fn single_quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[cfg(unix)]
pub async fn launch_own_group(launch: OwnGroupLaunch) -> Result<GatedChild, LaunchBarrierError> {
    let options = launch.options.clone().unwrap_or_default();
    let caller_stdio = options.stdio;
    let fd = control_fd(&caller_stdio);
    // `withControlPipe` pads and appends the control entry; its position is the descriptor the shim opens.
    debug_assert_eq!(
        with_control_pipe(&caller_stdio, fd, StdioKind::Ignore, StdioKind::Pipe).len(),
        fd + 1
    );
    let control = Control::create(&std::env::temp_dir())?;
    let script = format!(
        "exec {fd}<>{}; {}",
        single_quoted(&control.path),
        go_shim_script(fd)
    );

    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .arg(&launch.program)
        .args(&launch.args);
    if let Some(cwd) = &options.cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = &options.env {
        command.env_clear().envs(env);
    }
    command
        .stdin(caller_stdio[0].stdio())
        .stdout(caller_stdio[1].stdio())
        .stderr(caller_stdio[2].stdio())
        .process_group(0);
    let mut shim: Child = command.spawn()?;
    let Some(pid) = shim.id() else {
        return Err(LaunchBarrierError::CouldNotStart(launch.program));
    };
    let stdin = shim.stdin.take();
    let stdout = shim.stdout.take();
    let stderr = shim.stderr.take();

    let (published, exited) = watch::channel(None::<ExitOutcome>);
    let gate = Arc::clone(&launch.gate);
    let gated = match gate.request(LaunchRequest {
        job_id: launch.job_id.clone(),
        origin: launch.origin,
        api: launch.api,
        containment: LaunchContainment::OwnGroup,
        lifetime: launch.lifetime.unwrap_or(LaunchLifetime::Owned),
        pid: Some(pid),
        pgid: Some(pid),
    }) {
        Ok(gated) => gated,
        Err(error) => {
            let _ = shim.start_kill();
            return Err(error.into());
        }
    };
    let launch_id = gated.launch_id.clone();

    // `void exited.then(... launch.gate.done ...)`: reaps the shim, reads its control report, then reports completion.
    let control = Arc::new(std::sync::Mutex::new(control));
    let waiter_control = Arc::clone(&control);
    let waiter_gate = Arc::clone(&gate);
    let waiter_id = launch_id.clone();
    tokio::spawn(async move {
        let status = shim.wait().await.ok();
        let enoent = waiter_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reported_enoent();
        let (exit_code, signal) = match &status {
            Some(status) => (status.code(), signal_name(status)),
            None => (None, None),
        };
        waiter_gate.done(&waiter_id, exit_code, signal.clone());
        let _ = published.send(Some(ExitOutcome {
            exit_code,
            signal,
            enoent,
        }));
    });

    // `await Promise.race([gated.acked, exited])`.
    if !gate.skips_ack_wait() {
        tokio::select! {
            _ = gated.acked => {}
            _ = wait_exited(exited.clone()) => {}
        }
    }
    if exited.borrow().is_none() {
        gate.invoke_now(&launch_id)?;
        control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .write_go();
    }
    Ok(GatedChild {
        launch_id,
        pid,
        stdin,
        stdout,
        stderr,
        exited,
    })
}

#[cfg(not(unix))]
pub async fn launch_own_group(_launch: OwnGroupLaunch) -> Result<GatedChild, LaunchBarrierError> {
    Err(LaunchBarrierError::PosixOnly)
}

/// A call that stays in the child's own group: ack first, then the invoke tick, then the call.
pub async fn gate_inherited<T>(
    gate: &LaunchGate,
    request: InheritedRequest,
    call: impl FnOnce() -> T,
) -> Result<(String, T), LaunchGateError> {
    let gated = gate.request(LaunchRequest {
        job_id: request.job_id,
        origin: request.origin,
        api: request.api,
        containment: LaunchContainment::Inherited,
        lifetime: LaunchLifetime::Owned,
        pid: None,
        pgid: None,
    })?;
    if !gate.skips_ack_wait() {
        // A dropped sender means the gate went away without an ack; the call must not run unacknowledged then.
        gated.acked.await.map_err(|_| {
            LaunchGateError::new("tool_child_launch_unacknowledged", &gated.launch_id)
        })?;
    }
    gate.invoke_now(&gated.launch_id)?;
    Ok((gated.launch_id, call()))
}

/// `{ jobId, origin, api }`.
#[derive(Debug, Clone)]
pub struct InheritedRequest {
    pub job_id: String,
    pub origin: LaunchOrigin,
    pub api: LaunchApi,
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::channel::ChannelSend;
    use crate::protocol::{ChildToServer, ServerToChild, ToolChildMessage};
    use std::time::Duration;
    use tokio::sync::mpsc;

    const BOUND: Duration = Duration::from_secs(30);

    /// The server end of a gate: records every frame the gate sends and lets the test acknowledge launches.
    struct Rig {
        gate: Arc<LaunchGate>,
        sent: mpsc::UnboundedReceiver<ToolChildMessage>,
    }

    struct RecordingChannel(mpsc::UnboundedSender<ToolChildMessage>);

    impl ChannelSend for RecordingChannel {
        fn send(&self, message: ToolChildMessage) -> bool {
            self.0.send(message).is_ok()
        }
    }

    fn rig(env: &[(&str, &str)]) -> Rig {
        let (tx, sent) = mpsc::unbounded_channel();
        let env = env
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let gate = LaunchGate::new(Arc::new(RecordingChannel(tx)), None, &env, None).expect("gate");
        Rig {
            gate: Arc::new(gate),
            sent,
        }
    }

    impl Rig {
        async fn next(&mut self) -> ToolChildMessage {
            tokio::time::timeout(BOUND, self.sent.recv())
                .await
                .expect("frame in time")
                .expect("channel open")
        }

        async fn next_launch_id(&mut self) -> String {
            match self.next().await {
                ToolChildMessage::Child(ChildToServer::Launch { launch_id, .. }) => launch_id,
                other => panic!("expected launch, got {other:?}"),
            }
        }

        fn ack(&self, launch_id: &str) {
            assert!(
                self.gate
                    .handle(&ToolChildMessage::Server(ServerToChild::LaunchAck {
                        launch_id: launch_id.to_string()
                    }))
            );
        }
    }

    fn launch_for(
        gate: &Arc<LaunchGate>,
        script: &str,
        lifetime: Option<LaunchLifetime>,
    ) -> OwnGroupLaunch {
        OwnGroupLaunch {
            gate: Arc::clone(gate),
            job_id: "j1".to_string(),
            origin: LaunchOrigin::Tool,
            api: LaunchApi::BunSpawn,
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), script.to_string()],
            options: None,
            lifetime,
        }
    }

    #[tokio::test]
    async fn holds_an_own_group_command_until_the_ack_then_runs_it_once_with_an_ordered_proof() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("ran");
        let mut rig = rig(&[]);
        let gate = Arc::clone(&rig.gate);
        let script = format!("touch '{}'", marker.display());
        let launching =
            tokio::spawn(async move { launch_own_group(launch_for(&gate, &script, None)).await });
        let launch_id = rig.next_launch_id().await;
        // Held: no invocation recorded and the command has not run.
        assert_eq!(rig.gate.clock.read(0).expect("slot").invocations, 0);
        assert!(!marker.exists());
        rig.ack(&launch_id);
        let child = tokio::time::timeout(BOUND, launching)
            .await
            .expect("launch in time")
            .expect("join")
            .expect("launch");
        assert_eq!(
            tokio::time::timeout(BOUND, child.exited())
                .await
                .expect("exit in time")
                .exit_code,
            Some(0)
        );
        assert!(marker.exists());
        let ToolChildMessage::Child(ChildToServer::LaunchProof {
            ack_tick,
            invoke_tick,
            ..
        }) = rig.next().await
        else {
            panic!("expected launch_proof first");
        };
        assert!(ack_tick.expect("ackTick") < invoke_tick);
        assert!(matches!(
            rig.next().await,
            ToolChildMessage::Child(ChildToServer::LaunchDone {
                exit_code: Some(0),
                ..
            })
        ));
    }

    #[tokio::test]
    async fn keeps_pid_and_group_across_exec_env_i() {
        let mut rig = rig(&[]);
        let gate = Arc::clone(&rig.gate);
        let launching = tokio::spawn(async move {
            launch_own_group(launch_for(&gate, "exec env -i /bin/sleep 300", None)).await
        });
        let ToolChildMessage::Child(ChildToServer::Launch {
            launch_id,
            pid,
            pgid,
            containment,
            lifetime,
            ..
        }) = rig.next().await
        else {
            panic!("expected launch");
        };
        assert_eq!(
            (containment, lifetime),
            (LaunchContainment::OwnGroup, LaunchLifetime::Owned)
        );
        assert_eq!(pid, pgid);
        rig.ack(&launch_id);
        let child = tokio::time::timeout(BOUND, launching)
            .await
            .expect("launch in time")
            .expect("join")
            .expect("launch");
        assert_eq!(Some(i64::from(child.pid)), pid);
        // End the group the way a cancelled job does and wait for the exit it causes.
        assert_eq!(rig.gate.end_job_groups("j1"), vec![child.pid]);
        let outcome = tokio::time::timeout(BOUND, child.exited())
            .await
            .expect("exit in time");
        assert_eq!(outcome.signal.as_deref(), Some("SIGKILL"));
    }

    #[tokio::test]
    async fn skip_ack_wait_runs_the_inherited_call_while_the_ack_is_held() {
        let mut rig = rig(&[("SENPI_TOOL_CHILD_TEST_SKIP_ACK_WAIT", "1")]);
        let mut calls = 0;
        let (launch_id, value) = gate_inherited(
            &rig.gate,
            InheritedRequest {
                job_id: "j1".into(),
                origin: LaunchOrigin::Tool,
                api: LaunchApi::CpExecFileSync,
            },
            || {
                calls += 1;
                calls
            },
        )
        .await
        .expect("gated");
        assert_eq!(value, 1);
        assert_eq!(rig.next_launch_id().await, launch_id);
        let ToolChildMessage::Child(ChildToServer::LaunchProof { ack_tick, .. }) = rig.next().await
        else {
            panic!("expected proof")
        };
        assert_eq!(ack_tick, None);
    }

    #[tokio::test]
    async fn gates_an_inherited_call_no_invocation_while_held_exactly_one_after() {
        let mut rig = rig(&[]);
        let gate = Arc::clone(&rig.gate);
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counted = Arc::clone(&calls);
        let gated = tokio::spawn(async move {
            gate_inherited(
                &gate,
                InheritedRequest {
                    job_id: "j1".into(),
                    origin: LaunchOrigin::Tool,
                    api: LaunchApi::CpExecFileSync,
                },
                || counted.fetch_add(1, Ordering::SeqCst) + 1,
            )
            .await
        });
        let launch_id = rig.next_launch_id().await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(rig.gate.clock.read(0).expect("slot").invocations, 0);
        rig.ack(&launch_id);
        let (_, value) = tokio::time::timeout(BOUND, gated)
            .await
            .expect("gate in time")
            .expect("join")
            .expect("gated");
        assert_eq!(value, 1);
        assert_eq!(rig.gate.clock.read(0).expect("slot").invocations, 1);
    }

    #[tokio::test]
    async fn reports_a_missing_program_as_enoent_after_the_go_line() {
        let mut rig = rig(&[]);
        let gate = Arc::clone(&rig.gate);
        let launching = tokio::spawn(async move {
            let mut launch = launch_for(&gate, "", None);
            launch.program = "maho-definitely-missing-program".to_string();
            launch.args = vec!["x".to_string()];
            launch_own_group(launch).await
        });
        let launch_id = rig.next_launch_id().await;
        rig.ack(&launch_id);
        let child = tokio::time::timeout(BOUND, launching)
            .await
            .expect("launch in time")
            .expect("join")
            .expect("launch");
        let outcome = tokio::time::timeout(BOUND, child.exited())
            .await
            .expect("exit in time");
        assert_eq!((outcome.exit_code, outcome.enoent), (Some(127), true));
    }
}
