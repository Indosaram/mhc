use std::{collections::BTreeMap, io::Write, path::Path, process::Stdio};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use super::{run_artifacts::{ChildExit, RunLaunchManifest, RunOutcome, read_run_json, unlink_run_artifact, update_run_ledger}, supervisor_process_identity::{SupervisorRuntimePlatform, get_supervisor_process_start, get_supervisor_runtime_platform, parse_supervisor_child_exit, read_supervisor_clock_now, schedule_supervisor_deadline, terminate_supervisor_child_hard, signal_supervisor_process_group}};

fn wall_now() -> f64 { chrono::Utc::now().timestamp_millis() as f64 }
fn runtime_platform(env: &BTreeMap<String, String>) -> SupervisorRuntimePlatform {
    get_supervisor_runtime_platform(env, if cfg!(windows) { "win32" } else { "posix" })
}
fn exit_status(status: std::process::ExitStatus) -> ChildExit {
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().and_then(|number| nix::sys::signal::Signal::try_from(number).ok()).map(|signal| signal.to_string())
    };
    #[cfg(not(unix))]
    let signal = None;
    ChildExit { code: status.code(), signal }
}
fn write_bootstrap_status(status: &serde_json::Value) -> std::io::Result<()> {
    let mut output = std::io::stdout().lock();
    match writeln!(output, "{status}").and_then(|()| output.flush()) {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        result => result,
    }
}

#[cfg(unix)]
struct TerminationSignals { term: tokio::signal::unix::Signal, interrupt: tokio::signal::unix::Signal }
#[cfg(unix)]
impl TerminationSignals {
    fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        Ok(Self { term: signal(SignalKind::terminate())?, interrupt: signal(SignalKind::interrupt())? })
    }
    async fn receive(&mut self) -> std::io::Result<i32> {
        tokio::select! { _ = self.term.recv() => Ok(143), _ = self.interrupt.recv() => Ok(130) }
    }
}
#[cfg(not(unix))]
struct TerminationSignals;
#[cfg(not(unix))]
impl TerminationSignals {
    fn new() -> std::io::Result<Self> { Ok(Self) }
    async fn receive(&mut self) -> std::io::Result<i32> {
        tokio::signal::ctrl_c().await?;
        Ok(130)
    }
}

/// The native bootstrap uses its stdout pipe for the source's fd 3 status
/// channel. Model stdout/stderr are opened separately from the launch manifest.
pub async fn run_child_bootstrap(run_dir: &Path) -> Result<(), String> {
    let mut signals = TerminationSignals::new().map_err(|error| error.to_string())?;
    let mut release = [0_u8; 1];
    if tokio::io::stdin().read(&mut release).await.map_err(|error| error.to_string())? == 0 { return Ok(()); }
    let manifest: RunLaunchManifest = read_run_json(&run_dir.join("launch.json")).map_err(|error| error.to_string())?;
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let mut command = tokio::process::Command::new(&manifest.command);
    command.args(&manifest.args).current_dir(&manifest.cwd).env_clear().envs(&manifest.env).kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&manifest.stdout_path).map_err(|error| error.to_string())?)
        .stderr(std::fs::File::create(&manifest.stderr_path).map_err(|error| error.to_string())?);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            write_bootstrap_status(&serde_json::json!({"code":null,"signal":null})).map_err(|error| error.to_string())?;
            return Ok(());
        }
    };
    write_bootstrap_status(&serde_json::json!({"code":child.id(),"signal":"MODEL_PID"})).map_err(|error| error.to_string())?;
    let own_pid = std::process::id();
    let term_env = env.clone();
    let (graceful_sent, mut graceful_received) = tokio::sync::oneshot::channel();
    let mut cancel_term = schedule_supervisor_deadline(manifest.hard_deadline_at, &env, wall_now, move || {
        if runtime_platform(&term_env) == SupervisorRuntimePlatform::Posix {
            if let Err(error) = signal_supervisor_process_group(own_pid, "SIGTERM", &term_env) { eprintln!("{error}"); }
        } else if graceful_sent.send(()).is_err() {
            eprintln!("bootstrap graceful deadline receiver closed");
        }
    }).map_err(|error| error.to_string())?;
    let kill_env = env.clone();
    let mut cancel_kill = schedule_supervisor_deadline(manifest.hard_deadline_at + manifest.termination_grace_ms, &env, wall_now, move || {
        if let Err(error) = terminate_supervisor_child_hard(runtime_platform(&kill_env), Some(own_pid), false, &kill_env) { eprintln!("{error}"); }
    }).map_err(|error| error.to_string())?;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status.map_err(|error| error.to_string()),
            signal = signals.receive() => {
                signal.map_err(|error| error.to_string())?;
                if runtime_platform(&env) == SupervisorRuntimePlatform::Win32 || cfg!(windows) {
                    signal_model_gracefully(&mut child)?;
                }
            }
            _ = &mut graceful_received, if runtime_platform(&env) == SupervisorRuntimePlatform::Win32 => {
                signal_model_gracefully(&mut child)?;
                break child.wait().await.map_err(|error| error.to_string());
            }
        }
    };
    cancel_term.cancel();
    cancel_kill.cancel();
    let status = exit_status(status?);
    write_bootstrap_status(&serde_json::json!({"code":status.code,"signal":status.signal})).map_err(|error| error.to_string())
}

fn signal_model_gracefully(child: &mut tokio::process::Child) -> Result<(), String> {
    #[cfg(unix)]
    {
        let Some(pid) = child.id() else { return Ok(()); };
        match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), nix::sys::signal::Signal::SIGTERM) {
            Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
    #[cfg(not(unix))]
    { child.start_kill().map_err(|error| error.to_string()) }
}

struct ContainedBootstrap { pid: Option<u32>, env: BTreeMap<String, String> }
impl Drop for ContainedBootstrap {
    fn drop(&mut self) {
        if let Err(error) = terminate_supervisor_child_hard(runtime_platform(&self.env), self.pid, true, &self.env) { eprintln!("{error}"); }
    }
}

/// The terminal lock remains supplied by the native owner until memory-core
/// exposes the upstream's unbounded cross-process gate.
pub async fn run_supervisor(
    run_dir: &Path, executable: &Path, prefix: &[String],
    gate: impl FnOnce(&mut dyn FnMut() -> Result<(), String>) -> Result<(), String>,
) -> Result<(), String> {
    let mut signals = TerminationSignals::new().map_err(|error| error.to_string())?;
    let launch_path = run_dir.join("launch.json");
    let ledger_path = run_dir.join("ledger.json");
    let manifest: RunLaunchManifest = read_run_json(&launch_path).map_err(|error| error.to_string())?;
    update_run_ledger(&ledger_path, &serde_json::Map::from_iter([
        ("launching".into(), false.into()), ("pid".into(), std::process::id().into()),
        ("processStart".into(), get_supervisor_process_start(std::process::id()).into()),
    ])).map_err(|error| error.to_string())?;
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let mut command = tokio::process::Command::new(executable);
    command.args(prefix).arg("--child-bootstrap").arg(run_dir)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(unix)]
    command.process_group(0);
    let mut bootstrap = command.spawn().map_err(|error| error.to_string())?;
    let pid = bootstrap.id().ok_or("child bootstrap did not receive a pid")?;
    let mut containment = ContainedBootstrap { pid: Some(pid), env: env.clone() };
    update_run_ledger(&ledger_path, &serde_json::Map::from_iter([
        ("childPid".into(), pid.into()), ("childProcessStart".into(), get_supervisor_process_start(pid).into()),
    ])).map_err(|error| error.to_string())?;
    let control = bootstrap.stdout.take().ok_or("child bootstrap status pipe is unavailable")?;
    let mut release = bootstrap.stdin.take().ok_or("child bootstrap release pipe is unavailable")?;
    release.write_all(b"1\n").await.map_err(|error| error.to_string())?;
    release.shutdown().await.map_err(|error| error.to_string())?;
    drop(release);
    let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let expired = timed_out.clone();
    let term_env = env.clone();
    let mut cancel_term = schedule_supervisor_deadline(manifest.hard_deadline_at, &env, wall_now, move || {
        expired.store(true, std::sync::atomic::Ordering::SeqCst);
        let result = match runtime_platform(&term_env) {
            SupervisorRuntimePlatform::Posix => signal_supervisor_process_group(pid, "SIGTERM", &term_env),
            SupervisorRuntimePlatform::Win32 => super::supervisor_process_identity::record_supervisor_graceful_deadline(Some(pid), &term_env),
        };
        if let Err(error) = result { eprintln!("{error}"); }
    }).map_err(|error| error.to_string())?;
    let kill_env = env.clone();
    let model_pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(pid));
    let kill_pid = model_pid.clone();
    let (kill_finished, killed) = tokio::sync::oneshot::channel();
    let mut cancel_kill = schedule_supervisor_deadline(manifest.hard_deadline_at + manifest.termination_grace_ms, &env, wall_now, move || {
        let platform = runtime_platform(&kill_env);
        let target = if platform == SupervisorRuntimePlatform::Posix { pid } else { kill_pid.load(std::sync::atomic::Ordering::SeqCst) };
        if let Err(error) = terminate_supervisor_child_hard(platform, Some(target), false, &kill_env) { eprintln!("{error}"); }
        if kill_finished.send(()).is_err() { eprintln!("supervisor hard-kill receiver closed"); }
    }).map_err(|error| error.to_string())?;
    let read_status = async {
        let mut text = String::new();
        let mut lines = tokio::io::BufReader::new(control).lines();
        while let Some(line) = lines.next_line().await.map_err(|error| error.to_string())? {
            if let Ok(message) = serde_json::from_str::<serde_json::Value>(&line)
                && message["signal"] == "MODEL_PID"
                && let Some(pid) = message["code"].as_u64().and_then(|pid| u32::try_from(pid).ok()) {
                model_pid.store(pid, std::sync::atomic::Ordering::SeqCst);
            }
            text.push_str(&line);
            text.push('\n');
        }
        Ok::<_, String>(text)
    };
    let (wrapper_exit, status_text) = tokio::select! {
        result = async { tokio::join!(bootstrap.wait(), read_status) } => result,
        signal = signals.receive() => return Err(format!("memory supervisor interrupted with {}", signal.map_err(|error| error.to_string())?)),
    };
    let status_text = status_text?;
    let parsed = parse_supervisor_child_exit(&status_text);
    if runtime_platform(&env) == SupervisorRuntimePlatform::Win32
        && timed_out.load(std::sync::atomic::Ordering::SeqCst) && parsed.is_none() {
        killed.await.map_err(|error| error.to_string())?;
    }
    cancel_term.cancel();
    cancel_kill.cancel();
    let wrapper_exit = exit_status(wrapper_exit.map_err(|error| error.to_string())?);
    containment.pid = None;
    let child_exit = match parsed {
        Some(status) => ChildExit { code: status.code.map(|code| code as i32), signal: status.signal },
        None => wrapper_exit,
    };
    let clock_now = read_supervisor_clock_now(&env, wall_now);
    let outcome = RunOutcome {
        version: 1, run_id: manifest.run_id.clone(), attempt: Some(manifest.attempt),
        finished_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true), child_exit,
        timed_out: timed_out.load(std::sync::atomic::Ordering::SeqCst) || (clock_now.is_finite() && clock_now >= manifest.hard_deadline_at),
    };
    super::run_outcome_publication::publish_run_outcome(run_dir, &manifest, &outcome, gate)?;
    unlink_run_artifact(&launch_path).map_err(|error| error.to_string())
}

pub async fn run_entry(
    args: &[String], executable: &Path, prefix: &[String],
    gate: impl FnOnce(&Path, &mut dyn FnMut() -> Result<(), String>) -> Result<(), String>,
) -> Result<(), String> {
    if args.first().is_some_and(|arg| arg == "--child-bootstrap") {
        let directory = args.get(1).ok_or("run directory is required")?;
        return run_child_bootstrap(Path::new(directory)).await;
    }
    let directory = Path::new(args.first().ok_or("run directory is required")?);
    run_supervisor(directory, executable, prefix, |operation| gate(directory, operation)).await
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use super::super::run_artifacts::{RunKind, write_run_json_atomic};

    fn fixture(root: &Path) -> RunLaunchManifest {
        let manifest = RunLaunchManifest {
            version: 1, run_id: "run".into(), attempt: 1, next_attempt: None,
            kind: RunKind::Facts, command: "/bin/sh".into(), args: vec![],
            cwd: root.to_string_lossy().into_owned(), env: BTreeMap::new(),
            hard_deadline_at: wall_now() + 60_000.0, termination_grace_ms: 100.0,
            max_output_bytes: 1024, stdout_path: root.join("stdout").to_string_lossy().into_owned(),
            stderr_path: root.join("stderr").to_string_lossy().into_owned(),
        };
        std::fs::write(&manifest.stdout_path, "").unwrap_or_else(|error|panic!("stdout fixture: {error}"));
        std::fs::write(&manifest.stderr_path, "").unwrap_or_else(|error|panic!("stderr fixture: {error}"));
        write_run_json_atomic(&root.join("launch.json"), &manifest, 0o600).unwrap_or_else(|error|panic!("launch fixture: {error}"));
        write_run_json_atomic(&root.join("ledger.json"), &serde_json::json!({"version":1,"runId":"run"}), 0o600).unwrap_or_else(|error|panic!("ledger fixture: {error}"));
        manifest
    }

    #[tokio::test]
    async fn release_handshake_records_identity_and_publishes_child_status() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path());
        let prefix = vec!["-c".into(), "read release; [ \"$release\" = 1 ] || exit 9; printf '%s\\n' '{\"code\":7,\"signal\":null}'".into()];
        run_supervisor(root.path(), Path::new("/bin/sh"), &prefix, |operation| operation()).await.unwrap();
        let outcome: RunOutcome = read_run_json(&root.path().join("outcome.json")).unwrap();
        assert_eq!(outcome.child_exit.code, Some(7));
        assert!(!outcome.timed_out);
        assert!(!root.path().join("launch.json").exists());
        let ledger: serde_json::Value = read_run_json(&root.path().join("ledger.json")).unwrap();
        assert_eq!(ledger["launching"], false);
        assert_eq!(ledger["pid"], std::process::id());
        assert!(ledger["childPid"].as_u64().is_some());
    }

    #[tokio::test]
    async fn absent_bootstrap_status_uses_wrapper_exit() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path());
        let prefix = vec!["-c".into(), "read release; exit 4".into()];
        run_supervisor(root.path(), Path::new("/bin/sh"), &prefix, |operation| operation()).await.unwrap();
        let outcome: RunOutcome = read_run_json(&root.path().join("outcome.json")).unwrap();
        assert_eq!(outcome.child_exit.code, Some(4));
    }

    #[tokio::test]
    async fn terminal_gate_failure_keeps_launch_and_does_not_publish() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path());
        let prefix = vec!["-c".into(), "read release; printf '%s\\n' '{\"code\":0,\"signal\":null}'".into()];
        let result = run_supervisor(root.path(), Path::new("/bin/sh"), &prefix, |_| Err("gate unavailable".into())).await;
        assert_eq!(result.unwrap_err(), "gate unavailable");
        assert!(root.path().join("launch.json").exists());
        assert!(!root.path().join("outcome.json").exists());
    }
    #[tokio::test]
    async fn entry_requires_directory_before_spawning_or_acquiring_gate() {
        assert_eq!(run_entry(&[], Path::new("unused"), &[], |_, _| panic!("no directory")).await.unwrap_err(), "run directory is required");
        assert_eq!(run_entry(&["--child-bootstrap".into()], Path::new("unused"), &[], |_, _| panic!("bootstrap never locks")).await.unwrap_err(), "run directory is required");
    }
}
