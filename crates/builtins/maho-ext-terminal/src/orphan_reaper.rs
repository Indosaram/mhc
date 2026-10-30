//! Port of senpi `terminal/orphan-reaper.ts`: confirm that a pid left by a crashed generation is
//! still that watcher before killing it on respawn. A pid alone is never trusted.
use crate::process_identity::{ChildProcessIdentity, process_boot_at_ms, same_boot};
use std::future::Future;
use std::pin::Pin;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerVerdict {
    Confirmed,
    Dead,
    Unverifiable,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessEvidence {
    pub started_at_ms: Option<f64>,
    /// The command line the OS reports for the pid, space-joined.
    pub argv: Option<String>,
    /// Linux only: raw NUL-separated `/proc/<pid>/environ`.
    pub environ: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Darwin,
    Win32,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::Darwin
        } else if cfg!(windows) {
            Self::Win32
        } else {
            Self::Other
        }
    }
}

pub type EvidenceFuture =
    Pin<Box<dyn Future<Output = std::io::Result<Option<ProcessEvidence>>> + Send>>;

#[derive(Default)]
pub struct OwnerProbes {
    pub platform: Option<Platform>,
    pub is_process_alive: Option<Box<dyn Fn(u32) -> bool + Send + Sync>>,
    pub read_process_evidence: Option<Box<dyn Fn(u32) -> EvidenceFuture + Send + Sync>>,
    pub boot_at_ms: Option<Box<dyn Fn() -> f64 + Send + Sync>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
}

/// Kill outcome of a signal: `Err(NotFound)` stands for ESRCH.
pub type KillFn = Box<dyn Fn(i64, Signal) -> std::io::Result<()> + Send + Sync>;

#[derive(Default)]
pub struct KillTreeOptions {
    pub grace_ms: Option<u64>,
    pub is_process_alive: Option<Box<dyn Fn(u32) -> bool + Send + Sync>>,
    pub kill: Option<KillFn>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReapAction {
    Killed,
    AlreadyDead,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReapResult {
    pub action: ReapAction,
    pub reason: Option<String>,
}

/// Start instants further apart than this are different processes, even on the same pid.
pub const OWNER_START_TOLERANCE_MS: f64 = 2_000.0;
pub const DEFAULT_KILL_GRACE_MS: u64 = 1_000;
/// `ps -o lstart=` is always `Www Mmm dd hh:mm:ss yyyy`; the command follows.
#[cfg(target_os = "macos")]
const LSTART_LENGTH: usize = 24;

/// EPERM: the pid exists but belongs to someone else - alive, and certainly not ours to kill.
pub fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        !matches!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None),
            Err(nix::errno::Errno::ESRCH)
        )
    }
    #[cfg(windows)]
    {
        maho_pty::process_alive(pid)
    }
}

#[cfg(target_os = "linux")]
async fn read_linux_evidence(pid: u32) -> std::io::Result<Option<ProcessEvidence>> {
    let started = maho_tool_child::group_ownership::read_process_start_time(pid)
        .await
        .and_then(|identity| maho_tool_child::group_ownership::process_start_time_ms(&identity));
    let environ = std::fs::read(format!("/proc/{pid}/environ"))?;
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))?;
    let argv = String::from_utf8_lossy(&cmdline)
        .split('\0')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Some(ProcessEvidence {
        started_at_ms: started,
        argv: Some(argv),
        environ: Some(String::from_utf8_lossy(&environ).into_owned()),
    }))
}

#[cfg(target_os = "macos")]
async fn read_darwin_evidence(pid: u32) -> std::io::Result<Option<ProcessEvidence>> {
    let output = std::process::Command::new("ps")
        .args(["-o", "lstart=,command=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other("ps failed"));
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if text.len() <= LSTART_LENGTH || !text.is_char_boundary(LSTART_LENGTH) {
        return Ok(None);
    }
    Ok(Some(ProcessEvidence {
        started_at_ms: maho_tool_child::group_ownership::process_start_time_ms(
            &text[..LSTART_LENGTH],
        ),
        argv: Some(text[LSTART_LENGTH..].trim().to_owned()),
        environ: None,
    }))
}

fn default_evidence(platform: Platform, pid: u32) -> EvidenceFuture {
    match platform {
        #[cfg(target_os = "linux")]
        Platform::Linux => Box::pin(read_linux_evidence(pid)),
        #[cfg(target_os = "macos")]
        Platform::Darwin => Box::pin(read_darwin_evidence(pid)),
        // Windows evidence comes from a PowerShell CIM query upstream; the native port has no
        // CIM client, and win32 never kills on a confirmation anyway, so it reports unverifiable.
        _ => {
            let _ = pid;
            Box::pin(async { Ok(None) })
        }
    }
}

fn argv_matches(observed: Option<&str>, argv: &[String]) -> bool {
    let (Some(observed), Some(command)) = (observed, argv.last()) else {
        return false;
    };
    observed.trim() == argv.join(" ")
        || (!command.is_empty() && observed.contains(command.as_str()))
}

/// `confirmOwner`: same boot, alive, same start instant and a per-platform content marker (env
/// on Linux, argv elsewhere; a background session without a monitor id is matched by argv).
pub async fn confirm_owner(
    runtime: &ChildProcessIdentity,
    monitor_id: Option<&str>,
    probes: &OwnerProbes,
) -> OwnerVerdict {
    let platform = probes.platform.unwrap_or_else(Platform::current);
    let boot = probes
        .boot_at_ms
        .as_ref()
        .map_or_else(|| process_boot_at_ms(None), |boot| boot());
    if !same_boot(runtime.boot_at_ms, Some(boot)) {
        return OwnerVerdict::Dead;
    }
    let alive = probes
        .is_process_alive
        .as_ref()
        .map_or_else(|| is_process_alive(runtime.pid), |alive| alive(runtime.pid));
    if !alive {
        return OwnerVerdict::Dead;
    }
    let evidence = match &probes.read_process_evidence {
        Some(read) => read(runtime.pid).await,
        None => default_evidence(platform, runtime.pid).await,
    };
    let Ok(Some(evidence)) = evidence else {
        return OwnerVerdict::Unverifiable;
    };
    let Some(started) = evidence.started_at_ms.filter(|value| value.is_finite()) else {
        return OwnerVerdict::Unverifiable;
    };
    if (started - runtime.started_at_ms).abs() > OWNER_START_TOLERANCE_MS {
        return OwnerVerdict::Unverifiable;
    }
    let marked = match (platform, monitor_id) {
        (Platform::Linux, Some(monitor_id)) => {
            let marker = format!("SENPI_MONITOR_ID={monitor_id}");
            evidence
                .environ
                .as_deref()
                .unwrap_or("")
                .split('\0')
                .any(|entry| entry == marker)
        }
        _ => argv_matches(evidence.argv.as_deref(), &runtime.argv),
    };
    if marked {
        OwnerVerdict::Confirmed
    } else {
        OwnerVerdict::Unverifiable
    }
}

fn default_kill(target: i64, signal: Signal) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let signal = match signal {
            Signal::Term => nix::sys::signal::Signal::SIGTERM,
            Signal::Kill => nix::sys::signal::Signal::SIGKILL,
        };
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(target as i32), signal).map_err(|errno| {
            if errno == nix::errno::Errno::ESRCH {
                std::io::Error::from(std::io::ErrorKind::NotFound)
            } else {
                std::io::Error::from_raw_os_error(errno as i32)
            }
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (target, signal);
        Err(std::io::Error::other(
            "process signals are unsupported on this platform",
        ))
    }
}

/// `killTree`: SIGTERM the group (or bare pid), wait the grace, SIGKILL if still alive.
pub async fn kill_tree(
    runtime: &ChildProcessIdentity,
    options: &KillTreeOptions,
) -> std::io::Result<ReapAction> {
    let target = runtime
        .process_group_id
        .map_or(i64::from(runtime.pid), |group| -i64::from(group));
    let kill = |target, signal| match &options.kill {
        Some(kill) => kill(target, signal),
        None => default_kill(target, signal),
    };
    match kill(target, Signal::Term) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReapAction::AlreadyDead);
        }
        Err(error) => return Err(error),
        Ok(()) => {}
    }
    tokio::time::sleep(std::time::Duration::from_millis(
        options.grace_ms.unwrap_or(DEFAULT_KILL_GRACE_MS),
    ))
    .await;
    let alive = options
        .is_process_alive
        .as_ref()
        .map_or_else(|| is_process_alive(runtime.pid), |alive| alive(runtime.pid));
    if alive {
        match kill(target, Signal::Kill) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    Ok(ReapAction::Killed)
}

/// `reapBeforeRespawn`.
pub async fn reap_before_respawn(
    runtime: &ChildProcessIdentity,
    monitor_id: &str,
    probes: &OwnerProbes,
    kill: &KillTreeOptions,
) -> std::io::Result<ReapResult> {
    let verdict = confirm_owner(runtime, Some(monitor_id), probes).await;
    if verdict == OwnerVerdict::Dead {
        return Ok(ReapResult {
            action: ReapAction::None,
            reason: None,
        });
    }
    // win32 has no process groups to signal and no race-free way to act on a confirmation.
    if verdict == OwnerVerdict::Confirmed
        && probes.platform.unwrap_or_else(Platform::current) != Platform::Win32
    {
        return Ok(ReapResult {
            action: kill_tree(runtime, kill).await?,
            reason: None,
        });
    }
    Ok(ReapResult {
        action: ReapAction::None,
        reason: Some(format!("previous watcher pid {} unverifiable", runtime.pid)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const BOOT: f64 = 1_700_000_000_000.0;
    const START: f64 = 1_700_000_100_000.0;

    fn runtime(argv: &[&str]) -> ChildProcessIdentity {
        ChildProcessIdentity {
            pid: 4242,
            process_group_id: Some(4242),
            started_at_ms: START,
            boot_at_ms: BOOT,
            argv: argv.iter().map(|part| (*part).to_owned()).collect(),
        }
    }

    fn probes(platform: Platform, alive: bool, evidence: Option<ProcessEvidence>) -> OwnerProbes {
        OwnerProbes {
            platform: Some(platform),
            is_process_alive: Some(Box::new(move |_: u32| alive)),
            read_process_evidence: Some(Box::new(move |_: u32| -> EvidenceFuture {
                let evidence = evidence.clone();
                Box::pin(async move { Ok(evidence) })
            })),
            boot_at_ms: Some(Box::new(|| BOOT)),
        }
    }

    fn evidence(started: f64, argv: &str, environ: Option<&str>) -> Option<ProcessEvidence> {
        Some(ProcessEvidence {
            started_at_ms: Some(started),
            argv: Some(argv.to_owned()),
            environ: environ.map(str::to_owned),
        })
    }

    #[tokio::test]
    async fn darwin_start_time_within_2_s_and_exact_argv() {
        let probe = probes(
            Platform::Darwin,
            true,
            evidence(START + 1_500.0, "sh -c tail -f log", None),
        );
        assert_eq!(
            confirm_owner(
                &runtime(&["sh", "-c", "tail -f log"]),
                Some("mon_1"),
                &probe
            )
            .await,
            OwnerVerdict::Confirmed
        );
    }

    #[tokio::test]
    async fn darwin_the_observed_command_contains_the_recorded_monitor_command() {
        let probe = probes(
            Platform::Darwin,
            true,
            evidence(START, "/bin/zsh tail -f log", None),
        );
        assert_eq!(
            confirm_owner(
                &runtime(&["sh", "-c", "tail -f log"]),
                Some("mon_1"),
                &probe
            )
            .await,
            OwnerVerdict::Confirmed
        );
    }

    #[tokio::test]
    async fn linux_env_marker_and_start_time() {
        let probe = probes(
            Platform::Linux,
            true,
            evidence(START, "other", Some("A=1\0SENPI_MONITOR_ID=mon_1\0")),
        );
        assert_eq!(
            confirm_owner(&runtime(&["x"]), Some("mon_1"), &probe).await,
            OwnerVerdict::Confirmed
        );
    }

    #[tokio::test]
    async fn linux_a_background_session_is_confirmed_by_argv() {
        let probe = probes(
            Platform::Linux,
            true,
            evidence(START, "sh -c sleep 9", Some("")),
        );
        assert_eq!(
            confirm_owner(&runtime(&["sh", "-c", "sleep 9"]), None, &probe).await,
            OwnerVerdict::Confirmed
        );
    }

    #[tokio::test]
    async fn linux_a_different_monitors_marker_is_not_the_owner() {
        let probe = probes(
            Platform::Linux,
            true,
            evidence(START, "x", Some("SENPI_MONITOR_ID=mon_10\0")),
        );
        assert_eq!(
            confirm_owner(&runtime(&["x"]), Some("mon_1"), &probe).await,
            OwnerVerdict::Unverifiable
        );
    }

    #[tokio::test]
    async fn alive_but_start_time_10_s_off_is_unverifiable_with_reason() {
        let probe = probes(
            Platform::Darwin,
            true,
            evidence(START + 10_000.0, "x", None),
        );
        let result = reap_before_respawn(
            &runtime(&["x"]),
            "mon_1",
            &probe,
            &KillTreeOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            ReapResult {
                action: ReapAction::None,
                reason: Some("previous watcher pid 4242 unverifiable".into())
            }
        );
    }

    #[tokio::test]
    async fn alive_start_time_matches_argv_differs_is_unverifiable() {
        let probe = probes(
            Platform::Darwin,
            true,
            evidence(START, "something else", None),
        );
        assert_eq!(
            confirm_owner(&runtime(&["sh", "-c", "tail"]), Some("mon_1"), &probe).await,
            OwnerVerdict::Unverifiable
        );
    }

    #[tokio::test]
    async fn a_probe_that_fails_or_has_no_evidence_is_unverifiable() {
        let mut probe = probes(Platform::Darwin, true, None);
        assert_eq!(
            confirm_owner(&runtime(&["x"]), Some("mon_1"), &probe).await,
            OwnerVerdict::Unverifiable
        );
        probe.read_process_evidence = Some(Box::new(|_: u32| -> EvidenceFuture {
            Box::pin(async { Err(std::io::Error::other("boom")) })
        }));
        assert_eq!(
            confirm_owner(&runtime(&["x"]), Some("mon_1"), &probe).await,
            OwnerVerdict::Unverifiable
        );
    }

    #[tokio::test]
    async fn esrch_is_dead_with_no_kill_and_no_reason() {
        let probe = probes(Platform::Linux, false, None);
        let result = reap_before_respawn(
            &runtime(&["x"]),
            "mon_1",
            &probe,
            &KillTreeOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            ReapResult {
                action: ReapAction::None,
                reason: None
            }
        );
    }

    #[tokio::test]
    async fn boot_mismatch_is_dead_without_probing_the_recycled_pid() {
        let probed = Arc::new(Mutex::new(false));
        let seen = probed.clone();
        let mut probe = probes(Platform::Linux, true, None);
        probe.boot_at_ms = Some(Box::new(|| BOOT + 600_000.0));
        probe.is_process_alive = Some(Box::new(move |_: u32| {
            *seen.lock().unwrap() = true;
            true
        }));
        assert_eq!(
            confirm_owner(&runtime(&["x"]), Some("mon_1"), &probe).await,
            OwnerVerdict::Dead
        );
        assert!(!*probed.lock().unwrap());
    }

    #[tokio::test]
    async fn win32_a_confirmed_owner_is_reported_but_never_killed() {
        let probe = probes(Platform::Win32, true, evidence(START, "x", None));
        let killed = Arc::new(Mutex::new(Vec::new()));
        let log = killed.clone();
        let options = KillTreeOptions {
            kill: Some(Box::new(move |target: i64, signal: Signal| {
                log.lock().unwrap().push((target, signal));
                Ok(())
            })),
            ..Default::default()
        };
        let result = reap_before_respawn(&runtime(&["x"]), "mon_1", &probe, &options)
            .await
            .unwrap();
        assert_eq!(result.action, ReapAction::None);
        assert!(killed.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn kill_tree_signals_the_bare_pid_and_skips_sigkill_once_it_is_gone() {
        let mut identity = runtime(&["x"]);
        identity.process_group_id = None;
        let killed = Arc::new(Mutex::new(Vec::new()));
        let log = killed.clone();
        let options = KillTreeOptions {
            grace_ms: Some(5),
            is_process_alive: Some(Box::new(|_: u32| false)),
            kill: Some(Box::new(move |target: i64, signal: Signal| {
                log.lock().unwrap().push((target, signal));
                Ok(())
            })),
        };
        assert_eq!(
            kill_tree(&identity, &options).await.unwrap(),
            ReapAction::Killed
        );
        assert_eq!(*killed.lock().unwrap(), vec![(4242, Signal::Term)]);
    }

    #[tokio::test]
    async fn kill_tree_reports_already_dead_when_sigterm_hits_esrch() {
        let options = KillTreeOptions {
            kill: Some(Box::new(|_: i64, _: Signal| {
                Err(std::io::ErrorKind::NotFound.into())
            })),
            ..Default::default()
        };
        assert_eq!(
            kill_tree(&runtime(&["x"]), &options).await.unwrap(),
            ReapAction::AlreadyDead
        );
    }
}
