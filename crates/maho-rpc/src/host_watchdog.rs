use std::{collections::HashMap, path::PathBuf};
pub const HOST_WATCH_FD_ENV: &str = "SENPI_RPC_HOST_WATCH_FD";
pub const HOST_SCRATCH_DIR_ENV: &str = "SENPI_RPC_HOST_SCRATCH_DIR";
pub const HOST_PUBLIC_SOCKET_ENV: &str = "SENPI_RPC_HOST_PUBLIC_SOCKET";
pub const HOST_WATCH_PPID_ENV: &str = "SENPI_RPC_HOST_WATCH_PPID";
/// The supervisor's recorded start time (off Linux), compared rarely by the poll.
pub const HOST_WATCH_PSTART_ENV: &str = "SENPI_RPC_HOST_WATCH_PSTART";
/// Linux: the supervisor's `/proc` start ticks, compared on the boot clock (senpi#3816 review).
pub const HOST_WATCH_PSTART_TICKS_ENV: &str = "SENPI_RPC_HOST_WATCH_PSTART_TICKS";
pub use crate::host_lifetime_binding::HOST_WATCH_LIFETIME_FIFO_ENV;
pub use crate::host_supervisor_poll::{
    HOST_WATCH_BACKSTOP_INTERVAL_MS, HOST_WATCH_IDENTITY_INTERVAL_MS,
    HOST_WATCH_IDENTITY_ONLY_INTERVAL_MS, HOST_WATCH_PPID_BACKUP_INTERVAL_MS,
    HOST_WATCH_PPID_INTERVAL_MS, HOST_WATCH_SPAWNING_IDENTITY_INTERVAL_MS,
};
/// The ppid poll cadence: slow while the pipe is armed, fast when it is absent or unusable.
pub const fn ppid_interval_ms(pipe_armed: bool) -> u64 {
    if pipe_armed {
        HOST_WATCH_PPID_BACKUP_INTERVAL_MS
    } else {
        HOST_WATCH_PPID_INTERVAL_MS
    }
}
pub const HOST_CLEANUP_PATHS_ENV: &str = "SENPI_RPC_HOST_CLEANUP_PATHS";
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostWatchdogConfig {
    pub fd: Option<u64>,
    pub ppid: Option<u64>,
    pub process_start_time: Option<String>,
    pub process_start_ticks: Option<String>,
    pub lifetime_fifo: Option<PathBuf>,
    /// macOS: the reopened stdin descriptor the internal bootstrap hands over (never read from the env).
    pub lifetime_fd: Option<i32>,
    pub scratch_dir: Option<PathBuf>,
    pub cleanup_paths: Option<Vec<PathBuf>>,
    pub public_socket: Option<String>,
}
fn positive_integer(value: Option<&String>) -> Option<u64> {
    let value = value?.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0 && *value <= 9007199254740991)
}
pub fn read_host_watchdog_config(env: &HashMap<String, String>) -> Option<HostWatchdogConfig> {
    read_host_watchdog_config_on(env, cfg!(windows))
}
/// `win32` keeps the inherited fd binding; a POSIX supervised child (ppid set) never arms slot three, which a
/// launcher may have closed or reused after wrappers/re-exec.
pub fn read_host_watchdog_config_on(
    env: &HashMap<String, String>,
    win32: bool,
) -> Option<HostWatchdogConfig> {
    let ppid = positive_integer(env.get(HOST_WATCH_PPID_ENV));
    let fd = if !win32 && ppid.is_some() {
        None
    } else {
        positive_integer(env.get(HOST_WATCH_FD_ENV))
    };
    if fd.is_none() && ppid.is_none() {
        return None;
    }
    let non_empty = |name: &str| env.get(name).filter(|value| !value.is_empty()).cloned();
    Some(HostWatchdogConfig {
        fd,
        ppid,
        process_start_time: non_empty(HOST_WATCH_PSTART_ENV),
        process_start_ticks: non_empty(HOST_WATCH_PSTART_TICKS_ENV),
        lifetime_fifo: non_empty(HOST_WATCH_LIFETIME_FIFO_ENV).map(PathBuf::from),
        lifetime_fd: None,
        scratch_dir: env
            .get(HOST_SCRATCH_DIR_ENV)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from),
        cleanup_paths: env.get(HOST_CLEANUP_PATHS_ENV).map(|paths| {
            paths
                .split('\n')
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .collect()
        }),
        public_socket: env
            .get(HOST_PUBLIC_SOCKET_ENV)
            .filter(|path| !path.is_empty())
            .cloned(),
    })
}
/// The watchdog config a normal launch reads, resolving branded aliases for every canonical
/// `SENPI_RPC_HOST_*` name (senpi `readHostWatchdogConfigFromBrandEnv`).
pub fn read_host_watchdog_config_from_brand_env(
    env: &HashMap<String, String>,
) -> Option<HostWatchdogConfig> {
    let mut canonical = HashMap::new();
    for (name, suffix) in [
        (HOST_WATCH_FD_ENV, "RPC_HOST_WATCH_FD"),
        (HOST_WATCH_PPID_ENV, "RPC_HOST_WATCH_PPID"),
        (HOST_WATCH_PSTART_ENV, "RPC_HOST_WATCH_PSTART"),
        (HOST_WATCH_PSTART_TICKS_ENV, "RPC_HOST_WATCH_PSTART_TICKS"),
        (HOST_WATCH_LIFETIME_FIFO_ENV, "RPC_HOST_LIFETIME_FIFO"),
        (HOST_SCRATCH_DIR_ENV, "RPC_HOST_SCRATCH_DIR"),
        (HOST_PUBLIC_SOCKET_ENV, "RPC_HOST_PUBLIC_SOCKET"),
        (HOST_CLEANUP_PATHS_ENV, "RPC_HOST_CLEANUP_PATHS"),
    ] {
        let value = env
            .get(name)
            .cloned()
            .or_else(|| maho_core::brand::env_value(suffix, env));
        if let Some(value) = value {
            canonical.insert(name.to_owned(), value);
        }
    }
    read_host_watchdog_config(&canonical)
}
/// Every binding suffix a supervisor sets; scrubbed once the binding is consumed (senpi `WATCH_ENV_SUFFIXES`).
pub const WATCH_ENV_SUFFIXES: [&str; 8] = [
    "RPC_HOST_WATCH_FD",
    "RPC_HOST_WATCH_PPID",
    "RPC_HOST_WATCH_PSTART",
    "RPC_HOST_WATCH_PSTART_TICKS",
    "RPC_HOST_LIFETIME_FIFO",
    "RPC_HOST_SCRATCH_DIR",
    "RPC_HOST_PUBLIC_SOCKET",
    "RPC_HOST_CLEANUP_PATHS",
];
/// Every variable name `scrub_host_watchdog_env` removes: the canonical `SENPI_` name and each branded alias.
pub fn host_watchdog_env_names() -> Vec<String> {
    let profile = maho_core::brand::brand_profile();
    let mut names = Vec::new();
    for suffix in WATCH_ENV_SUFFIXES {
        names.push(format!("SENPI_{suffix}"));
        for name in maho_core::brand::brand_env_names(suffix, Some(&profile)) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}
/// Removes the supervisor binding from `env` (senpi#3815 review L4), so no tool, shell or nested host this host
/// starts inherits a binding to a supervisor that is not its own. The generation identity stays.
pub fn scrub_host_watchdog_env(env: &mut HashMap<String, String>) {
    for name in host_watchdog_env_names() {
        env.remove(&name);
    }
}
/// Reads the supervisor binding once and removes it from `env` (senpi `consumeHostWatchdogEnv`). The process
/// environment itself is scrubbed by the CLI entry (`maho-process-env`), which may write it safely.
pub fn consume_host_watchdog_env(env: &mut HashMap<String, String>) -> Option<HostWatchdogConfig> {
    let config = read_host_watchdog_config_from_brand_env(env);
    scrub_host_watchdog_env(env);
    config
}
/**
 * Arms the configured watchdog: `on_supervisor_gone` receives the reason and is expected to
 * run the host's normal clean shutdown. `capture_before_cleanup` runs first, while the supervisor's
 * private directory still exists, so the host can read state its shutdown needs - the public-socket
 * ownership token above all (senpi `armHostWatchdog`'s `beforeCleanup`). Returns the reason future;
 * a caller awaits it, or drops it to disarm.
 */
pub async fn arm_host_watchdog(
    config: Option<HostWatchdogConfig>,
    capture_before_cleanup: impl std::future::Future<Output = Result<(), String>>,
    mut on_supervisor_gone: impl FnMut(String),
) {
    let Some(config) = config else {
        return;
    };
    let platform = if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    };
    let recorded = crate::host_supervisor_poll::RecordedStart {
        ticks: config.process_start_ticks.clone(),
        time: config.process_start_time.clone(),
    };
    let sole = crate::host_supervisor_poll::sole_interval_ms(&recorded);
    // The poll stays disarmed until a cadence is chosen below; only the recorded supervisor is polled.
    let (arm_poll, poll_interval) = tokio::sync::watch::channel(None::<u64>);
    let poll = async {
        match config.ppid {
            Some(pid) => {
                crate::host_supervisor_poll::watch_supervisor_poll(
                    u32::try_from(pid).unwrap_or(u32::MAX),
                    recorded.clone(),
                    poll_interval,
                    crate::host_supervisor_poll::OsSupervisorProbe,
                )
                .await
            }
            None => std::future::pending().await,
        }
    };
    let mut pipe = config.fd.and_then(watch_fd_for_eof);
    let lifetime = crate::host_lifetime_binding::arm_lifetime_binding(
        &crate::host_lifetime_binding::LifetimeBinding {
            lifetime_fifo: config.lifetime_fifo.clone(),
            lifetime_fd: config.lifetime_fd,
        },
        platform,
    );
    // With an event binding the poll is one long backstop; without one it backs up a live pipe (senpi#3738) or is
    // the only binding at its sole cadence.
    let mut poll_wanted = lifetime.is_none();
    let fallback_interval = |pipe_live: bool| {
        if pipe_live {
            HOST_WATCH_PPID_BACKUP_INTERVAL_MS
        } else {
            sole
        }
    };
    let _ = arm_poll.send(Some(if poll_wanted {
        fallback_interval(pipe.is_some())
    } else {
        HOST_WATCH_BACKSTOP_INTERVAL_MS
    }));
    let mut lifetime = lifetime;
    tokio::pin!(poll);
    let reason = loop {
        tokio::select! {
            result = next_event(&mut pipe) => match result {
                Ok(reason) => break reason,
                // The pipe failed later: the poll returns to its sole cadence when it is the binding.
                Err(_) => {
                    pipe = None;
                    if poll_wanted {
                        let _ = arm_poll.send(Some(sole));
                    }
                }
            },
            event = next_event(&mut lifetime) => match event {
                Ok(crate::host_lifetime_binding::LifetimeEvent::Gone(reason)) => break reason,
                // A failed binding is unknown, not death: fall back to the poll.
                _ => {
                    lifetime = None;
                    poll_wanted = true;
                    let _ = arm_poll.send(Some(fallback_interval(pipe.is_some())));
                }
            },
            reason = &mut poll => break reason,
        }
    };
    let _ = cleanup_watchdog_paths(&config, capture_before_cleanup).await;
    on_supervisor_gone(reason);
}
/// The next event of an armed one-shot binding; never resolves for an absent one.
async fn next_event<T>(
    receiver: &mut Option<tokio::sync::oneshot::Receiver<T>>,
) -> Result<T, tokio::sync::oneshot::error::RecvError> {
    match receiver.as_mut() {
        Some(receiver) => receiver.await,
        None => std::future::pending().await,
    }
}
/// EOF on the inherited pipe is the primary signal; readable data is ignored, only close matters.
///
/// The blocking read lives on its OWN detached OS thread, never on the runtime's blocking pool: a
/// pooled read parked in `read(2)` is not cancelled when its task is dropped, and tokio joins every
/// blocking worker when the runtime drops, so a host that decides to exit on its own (an idle or
/// empty exit) would hang in that join until something killed it - the exact failure senpi
/// documents for a thread-pool read. A detached thread blocked on the pipe is terminated by process
/// exit and is never joined. A descriptor that cannot be reopened leaves the binding inert rather
/// than killing a healthy host.
fn watch_fd_for_eof(fd: u64) -> Option<tokio::sync::oneshot::Receiver<String>> {
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut file = std::fs::File::open(format!("/dev/fd/{fd}")).ok()?;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name(format!("rpc-host-watch-{fd}"))
            .spawn(move || {
                let mut buffer = [0u8; 1024];
                loop {
                    match file.read(&mut buffer) {
                        Ok(0) | Err(_) => {
                            let _ = sender.send(format!("supervisor pipe fd {fd} closed"));
                            return;
                        }
                        Ok(_) => {}
                    }
                }
            })
            .ok()?;
        Some(receiver)
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        None
    }
}
pub fn supervisor_gone_reason(
    supervisor_pid: u32,
    self_pid: u32,
    ppid: u32,
    alive: bool,
) -> Option<String> {
    if supervisor_pid == self_pid {
        return None;
    }
    (!alive || ppid != supervisor_pid)
        .then(|| format!("supervisor pid {supervisor_pid} is gone (ppid={ppid})"))
}
pub async fn watch_supervisor_pipe(
    mut input: impl tokio::io::AsyncRead + Unpin,
) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut buffer = [0; 1024];
    while input.read(&mut buffer).await? != 0 {}
    Ok(())
}
pub async fn watch_supervisor_parent(
    supervisor_pid: u32,
    self_pid: u32,
    read: impl FnMut() -> (u32, bool),
) -> String {
    watch_supervisor_parent_every(
        supervisor_pid,
        self_pid,
        std::time::Duration::from_millis(HOST_WATCH_PPID_INTERVAL_MS),
        read,
    )
    .await
}
pub async fn watch_supervisor_parent_every(
    supervisor_pid: u32,
    self_pid: u32,
    period: std::time::Duration,
    mut read: impl FnMut() -> (u32, bool),
) -> String {
    let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        timer.tick().await;
        let (ppid, alive) = read();
        if let Some(reason) = supervisor_gone_reason(supervisor_pid, self_pid, ppid, alive) {
            return reason;
        }
    }
}
pub async fn cleanup_watchdog_paths(
    config: &HostWatchdogConfig,
    before_cleanup: impl std::future::Future<Output = Result<(), String>>,
) {
    let _ = before_cleanup.await;
    for path in config
        .cleanup_paths
        .iter()
        .flatten()
        .chain(config.scratch_dir.iter())
    {
        let _ = std::fs::remove_dir_all(path).or_else(|_| std::fs::remove_file(path));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requires_lifetime_binding_and_filters_empty_paths() {
        assert!(read_host_watchdog_config(&HashMap::new()).is_none());
        let env = HashMap::from([
            (HOST_WATCH_FD_ENV.into(), " 3 ".into()),
            (HOST_CLEANUP_PATHS_ENV.into(), "a\n\nb".into()),
            (HOST_SCRATCH_DIR_ENV.into(), "".into()),
        ]);
        let config = read_host_watchdog_config(&env).unwrap();
        assert_eq!(config.fd, Some(3));
        assert_eq!(
            config.cleanup_paths.unwrap(),
            vec![PathBuf::from("a"), PathBuf::from("b")]
        );
        assert!(config.scratch_dir.is_none());
    }
    #[test]
    fn a_posix_supervised_child_ignores_slot_three_and_reads_the_start_identity() {
        let env = HashMap::from([
            (HOST_WATCH_FD_ENV.into(), "3".into()),
            (HOST_WATCH_PPID_ENV.into(), "42".into()),
            (HOST_WATCH_PSTART_ENV.into(), "123".into()),
            (HOST_WATCH_PSTART_TICKS_ENV.into(), "".into()),
            (
                HOST_WATCH_LIFETIME_FIFO_ENV.into(),
                "/tmp/x/lifetime.fifo".into(),
            ),
        ]);
        let posix = read_host_watchdog_config_on(&env, false).unwrap();
        assert_eq!(posix.fd, None);
        assert_eq!(posix.ppid, Some(42));
        assert_eq!(posix.process_start_time.as_deref(), Some("123"));
        assert_eq!(posix.process_start_ticks, None);
        assert_eq!(
            posix.lifetime_fifo,
            Some(PathBuf::from("/tmp/x/lifetime.fifo"))
        );
        assert_eq!(
            read_host_watchdog_config_on(&env, true).unwrap().fd,
            Some(3)
        );
    }
    #[test]
    fn consuming_the_binding_scrubs_every_watch_name_but_keeps_the_generation() {
        let mut env = HashMap::from([
            (HOST_WATCH_PPID_ENV.into(), "42".into()),
            (HOST_WATCH_LIFETIME_FIFO_ENV.into(), "/f".into()),
            (HOST_CLEANUP_PATHS_ENV.into(), "a".into()),
            ("SENPI_RPC_HOST_INSTANCE_ID".into(), "gen".into()),
        ]);
        let config = consume_host_watchdog_env(&mut env).unwrap();
        assert_eq!(config.ppid, Some(42));
        assert_eq!(env.len(), 1);
        assert!(env.contains_key("SENPI_RPC_HOST_INSTANCE_ID"));
        assert!(consume_host_watchdog_env(&mut env).is_none());
    }
    #[test]
    fn armed_pipe_slows_the_ppid_backup_cadence() {
        assert_eq!(ppid_interval_ms(true), 5_000);
        assert_eq!(ppid_interval_ms(false), 250);
    }
    #[test]
    fn self_binding_is_live_and_reparenting_is_loss() {
        assert!(supervisor_gone_reason(1, 1, 0, false).is_none());
        assert!(supervisor_gone_reason(2, 1, 2, true).is_none());
        assert!(supervisor_gone_reason(2, 1, 1, true).is_some());
    }
    #[tokio::test]
    async fn capture_runs_before_cleanup_even_when_it_fails() {
        let temp = tempfile::tempdir().unwrap();
        let scratch = temp.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let config = HostWatchdogConfig {
            fd: Some(3),
            ppid: None,
            scratch_dir: Some(scratch.clone()),
            cleanup_paths: None,
            public_socket: None,
            ..Default::default()
        };
        cleanup_watchdog_paths(&config, async {
            assert!(scratch.exists());
            Err("capture failed".into())
        })
        .await;
        assert!(!scratch.exists());
    }
}
