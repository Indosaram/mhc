//! Run a child with a byte-capped capture and a whole-tree timeout (SIGTERM, then SIGKILL survivors).

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTreeSignalTarget {
    Process,
    ProcessGroup,
    ProcessTree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTreeSignalOutcome {
    Denied,
    Failed,
    Missing,
    Sent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeSignal {
    Term,
    Kill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTreeSignalAttempt {
    pub error: Option<String>,
    pub outcome: ProcessTreeSignalOutcome,
    pub pid: u32,
    pub signal: TreeSignal,
    pub target: ProcessTreeSignalTarget,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessTreeTerminationReport {
    pub attempts: Vec<ProcessTreeSignalAttempt>,
    pub survivor_pids: Vec<u32>,
}

pub struct ProcessTreeRunOptions<'a> {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The complete child environment.
    pub env: HashMap<String, String>,
    pub max_buffer: usize,
    pub timeout_ms: u64,
    pub termination_grace_ms: Option<u64>,
    pub termination_wait_ms: Option<u64>,
    pub on_termination_report: Option<&'a dyn Fn(&ProcessTreeTerminationReport)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTreeRunResult {
    pub exit_code: i32,
    /// Terminating signal number on Unix.
    pub signal: Option<i32>,
    pub stderr: String,
    pub stdout: String,
    pub termination: Option<ProcessTreeTerminationReport>,
    pub timed_out: bool,
}

const DEFAULT_TERMINATION_GRACE_MS: u64 = 1_000;
const DEFAULT_TERMINATION_WAIT_MS: u64 = 2_000;

enum Event {
    Chunk {
        stdout: bool,
        bytes: Vec<u8>,
    },
    Closed {
        code: Option<i32>,
        signal: Option<i32>,
    },
}

fn pump(
    mut stream: impl Read + Send + 'static,
    stdout: bool,
    tx: mpsc::Sender<Event>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut chunk = [0_u8; 8192];
        while let Ok(read) = stream.read(&mut chunk) {
            if read == 0
                || tx
                    .send(Event::Chunk {
                        stdout,
                        bytes: chunk[..read].to_vec(),
                    })
                    .is_err()
            {
                break;
            }
        }
    })
}

#[cfg(unix)]
fn status_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn status_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Spawn `command` detached into its own process group and collect output up to `max_buffer`
/// bytes per stream; overflow exits 1 with no partial text, timeout exits 124.
pub fn run_process_with_tree_timeout(options: &ProcessTreeRunOptions<'_>) -> ProcessTreeRunResult {
    let mut command = Command::new(&options.command);
    command
        .args(&options.args)
        .current_dir(&options.cwd)
        .env_clear()
        .envs(&options.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ProcessTreeRunResult {
                exit_code: 1,
                signal: None,
                stderr: error.to_string(),
                stdout: String::new(),
                termination: None,
                timed_out: false,
            };
        }
    };
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    let pumps: Vec<_> = [
        child.stdout.take().map(|s| pump(s, true, tx.clone())),
        child.stderr.take().map(|s| pump(s, false, tx.clone())),
    ]
    .into_iter()
    .flatten()
    .collect();
    let (closed_tx, closed_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let status = child.wait();
        for handle in pumps {
            let _ = handle.join();
        }
        let (code, signal) = status.map_or((None, None), |status| {
            (status.code(), status_signal(&status))
        });
        let _ = closed_tx.send(());
        let _ = tx.send(Event::Closed { code, signal });
    });

    let deadline = Instant::now() + Duration::from_millis(options.timeout_ms);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let terminate = || {
        let report = terminate_process_tree(
            pid,
            &closed_rx,
            options
                .termination_grace_ms
                .unwrap_or(DEFAULT_TERMINATION_GRACE_MS),
            options
                .termination_wait_ms
                .unwrap_or(DEFAULT_TERMINATION_WAIT_MS),
        );
        if let Some(observer) = options.on_termination_report {
            observer(&report);
        }
        report
    };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(Event::Chunk {
                stdout: is_stdout,
                bytes,
            }) => {
                let target = if is_stdout { &mut stdout } else { &mut stderr };
                if target.len() + bytes.len() > options.max_buffer {
                    let report = terminate();
                    return finish(1, None, &stdout, &stderr, Some(report), false);
                }
                target.extend_from_slice(&bytes);
            }
            Ok(Event::Closed { code, signal }) => {
                return finish(code.unwrap_or(1), signal, &stdout, &stderr, None, false);
            }
            Err(RecvTimeoutError::Timeout) => {
                let report = terminate();
                let signal = drain_signal(&rx);
                return finish(124, signal, &stdout, &stderr, Some(report), true);
            }
            Err(RecvTimeoutError::Disconnected) => {
                return finish(1, None, &stdout, &stderr, None, false);
            }
        }
    }
}

fn drain_signal(rx: &Receiver<Event>) -> Option<i32> {
    rx.try_iter().find_map(|event| match event {
        Event::Closed { signal, .. } => signal,
        Event::Chunk { .. } => None,
    })
}

fn finish(
    exit_code: i32,
    signal: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
    termination: Option<ProcessTreeTerminationReport>,
    timed_out: bool,
) -> ProcessTreeRunResult {
    let (stdout, stderr) = if exit_code == 1 && termination.is_some() && !timed_out {
        (String::new(), String::from_utf8_lossy(stderr).into_owned())
    } else {
        (
            String::from_utf8_lossy(stdout).into_owned(),
            String::from_utf8_lossy(stderr).into_owned(),
        )
    };
    ProcessTreeRunResult {
        exit_code,
        signal,
        stderr,
        stdout,
        termination,
        timed_out,
    }
}

#[cfg(unix)]
fn send_signal(
    pid: u32,
    signal: TreeSignal,
    target: ProcessTreeSignalTarget,
) -> ProcessTreeSignalAttempt {
    let raw = match signal {
        TreeSignal::Term => libc::SIGTERM,
        TreeSignal::Kill => libc::SIGKILL,
    };
    let Ok(signed) = libc::pid_t::try_from(pid) else {
        return ProcessTreeSignalAttempt {
            error: Some("pid out of range".to_string()),
            outcome: ProcessTreeSignalOutcome::Failed,
            pid,
            signal,
            target,
        };
    };
    let target_pid = if target == ProcessTreeSignalTarget::ProcessGroup {
        -signed
    } else {
        signed
    };
    // SAFETY: kill(2) only reads its integer arguments.
    let result = unsafe { libc::kill(target_pid, raw) };
    if result == 0 {
        return ProcessTreeSignalAttempt {
            error: None,
            outcome: ProcessTreeSignalOutcome::Sent,
            pid,
            signal,
            target,
        };
    }
    let error = std::io::Error::last_os_error();
    let outcome = match error.raw_os_error() {
        Some(libc::ESRCH) => {
            return ProcessTreeSignalAttempt {
                error: None,
                outcome: ProcessTreeSignalOutcome::Missing,
                pid,
                signal,
                target,
            };
        }
        Some(libc::EPERM) => ProcessTreeSignalOutcome::Denied,
        _ => ProcessTreeSignalOutcome::Failed,
    };
    ProcessTreeSignalAttempt {
        error: Some(error.to_string()),
        outcome,
        pid,
        signal,
        target,
    }
}

#[cfg(unix)]
pub(crate) fn is_process_alive(pid: u32) -> bool {
    let Ok(signed) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 performs only the existence/permission check.
    if unsafe { libc::kill(signed, 0) } == 0 {
        return !is_zombie(pid);
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn is_zombie(pid: u32) -> bool {
    Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .is_ok_and(|output| {
            String::from_utf8_lossy(&output.stdout)
                .trim_start()
                .starts_with('Z')
        })
}

#[cfg(not(unix))]
pub(crate) fn is_process_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
}

#[cfg(unix)]
fn list_tree_pids(root: u32) -> BTreeSet<u32> {
    let mut selected = BTreeSet::from([root]);
    let Ok(output) = Command::new("ps")
        .args(["-eo", "pid=,ppid=,pgid="])
        .output()
    else {
        return selected;
    };
    let rows: Vec<(u32, u32, u32)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().map(str::parse::<u32>);
            match (fields.next(), fields.next(), fields.next(), fields.next()) {
                (Some(Ok(pid)), Some(Ok(ppid)), Some(Ok(pgid)), None) => Some((pid, ppid, pgid)),
                _ => None,
            }
        })
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for (pid, ppid, pgid) in &rows {
            if !selected.contains(pid) && (*pgid == root || selected.contains(ppid)) {
                selected.insert(*pid);
                changed = true;
            }
        }
    }
    selected
}

fn wait_closed(closed: &Receiver<()>, wait: Duration) {
    let _ = closed.recv_timeout(wait);
}

fn wait_for_survivors(pids: &[u32], wait_ms: u64) -> Vec<u32> {
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    let mut survivors: Vec<u32> = pids
        .iter()
        .copied()
        .filter(|pid| is_process_alive(*pid))
        .collect();
    while !survivors.is_empty() && Instant::now() < deadline {
        thread::sleep(
            Duration::from_millis(25).min(deadline.saturating_duration_since(Instant::now())),
        );
        survivors.retain(|pid| is_process_alive(*pid));
    }
    survivors
}

#[cfg(unix)]
fn terminate_process_tree(
    pid: u32,
    closed: &Receiver<()>,
    grace_ms: u64,
    wait_ms: u64,
) -> ProcessTreeTerminationReport {
    let mut known = list_tree_pids(pid);
    let mut attempts = vec![send_signal(
        pid,
        TreeSignal::Term,
        ProcessTreeSignalTarget::ProcessGroup,
    )];
    wait_closed(closed, Duration::from_millis(grace_ms));
    known.extend(list_tree_pids(pid));
    let graceful_survivors: Vec<u32> = known
        .iter()
        .copied()
        .filter(|p| is_process_alive(*p))
        .collect();
    if !graceful_survivors.is_empty() {
        attempts.push(send_signal(
            pid,
            TreeSignal::Kill,
            ProcessTreeSignalTarget::ProcessGroup,
        ));
        attempts.extend(
            graceful_survivors
                .iter()
                .map(|p| send_signal(*p, TreeSignal::Kill, ProcessTreeSignalTarget::Process)),
        );
    }
    let known: Vec<u32> = known.into_iter().collect();
    let survivor_pids = wait_for_survivors(&known, wait_ms);
    wait_closed(closed, Duration::from_millis(wait_ms));
    ProcessTreeTerminationReport {
        attempts,
        survivor_pids,
    }
}

#[cfg(not(unix))]
fn terminate_process_tree(
    pid: u32,
    closed: &Receiver<()>,
    grace_ms: u64,
    wait_ms: u64,
) -> ProcessTreeTerminationReport {
    let taskkill = |signal: TreeSignal| {
        let mut args = vec!["/PID".to_string(), pid.to_string(), "/T".to_string()];
        if signal == TreeSignal::Kill {
            args.push("/F".to_string());
        }
        match Command::new("taskkill.exe").args(&args).output() {
            Ok(output) if output.status.success() => ProcessTreeSignalAttempt {
                error: None,
                outcome: ProcessTreeSignalOutcome::Sent,
                pid,
                signal,
                target: ProcessTreeSignalTarget::ProcessTree,
            },
            Ok(output) => ProcessTreeSignalAttempt {
                error: Some(String::from_utf8_lossy(&output.stderr).into_owned()),
                outcome: ProcessTreeSignalOutcome::Failed,
                pid,
                signal,
                target: ProcessTreeSignalTarget::ProcessTree,
            },
            Err(error) => ProcessTreeSignalAttempt {
                error: Some(error.to_string()),
                outcome: ProcessTreeSignalOutcome::Failed,
                pid,
                signal,
                target: ProcessTreeSignalTarget::ProcessTree,
            },
        }
    };
    let mut attempts = vec![taskkill(TreeSignal::Term)];
    wait_closed(closed, Duration::from_millis(grace_ms));
    if is_process_alive(pid) {
        attempts.push(taskkill(TreeSignal::Kill));
    }
    let survivor_pids = wait_for_survivors(&[pid], wait_ms);
    wait_closed(closed, Duration::from_millis(wait_ms));
    ProcessTreeTerminationReport {
        attempts,
        survivor_pids,
    }
}
