pub use crate::host_child_exit::classify_child_exit;
use std::collections::HashMap;
pub const HOST_COLD_START_ENV: &str = "SENPI_RPC_HOST_COLD_START";
pub const HOST_IDLE_EXIT_MS_ENV: &str = "SENPI_RPC_HOST_IDLE_EXIT_MS";
pub const DEFAULT_HOST_IDLE_EXIT_MS: f64 = 900000.;
pub const HANDOFF_GRACE_MS_ENV: &str = "SENPI_RPC_HANDOFF_GRACE_MS";
pub const DEFAULT_HANDOFF_GRACE_MS: f64 = 600000.;
pub use crate::host_launch_spec::HostLifecyclePolicyInput;
pub const INTERNAL_SUPERVISOR_FLAG: &str = "--internal-rpc-host-supervisor";
/// The stdio slot senpi gives the child's lifetime pipe (senpi `CHILD_WATCH_FD`). Rust's `Command`
/// exposes only the three standard slots, so the pipe is inherited by descriptor instead - see
/// `bind_child_watch_pipe`.
pub const CHILD_WATCH_FD: u32 = 3;
/// Gives `child_env` the child's lifetime binding and returns the pipe ends this process keeps
/// alive for the whole supervisor run (senpi host-lifecycle.ts:513-544, stdio slot 3).
///
/// senpi duplicates the read end into stdio slot 3. Rust's `Command` exposes only the three
/// standard slots and `pre_exec` is unavailable under this crate's `unsafe_code = "forbid"`, so the
/// read end's close-on-exec flag is cleared instead: the child inherits the pipe at the number this
/// process holds it at, and `HOST_WATCH_FD_ENV` carries exactly that number. The write end stays
/// here, close-on-exec, is never inherited and is never written - only its close matters.
#[cfg(unix)]
fn bind_child_watch_pipe(
    child_env: &mut HashMap<String, String>,
) -> std::io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::AsRawFd;
    let (read_end, write_end) = rustix::pipe::pipe()?;
    rustix::io::fcntl_setfd(&write_end, rustix::io::FdFlags::CLOEXEC)
        .map_err(std::io::Error::from)?;
    rustix::io::fcntl_setfd(&read_end, rustix::io::FdFlags::empty())
        .map_err(std::io::Error::from)?;
    let descriptor = u64::try_from(read_end.as_raw_fd())
        .map_err(|_| std::io::Error::other("child watch descriptor is not representable"))?;
    child_env.insert(
        crate::host_watchdog::HOST_WATCH_FD_ENV.into(),
        descriptor.to_string(),
    );
    Ok((read_end, write_end))
}
#[derive(Clone, Copy, Default)]
pub struct HostActivity {
    pub connections: u64,
    pub active_turns: u64,
}
#[derive(Default)]
pub struct ObservedHostTurns {
    busy: HashMap<String, u64>,
}
impl ObservedHostTurns {
    pub async fn read_events(
        &mut self,
        mut input: impl tokio::io::AsyncRead + Unpin,
        mut changed: impl FnMut(&Self),
    ) -> std::io::Result<()> {
        use tokio::io::AsyncReadExt;
        let mut lines = crate::jsonl::JsonlLineReader::new(crate::jsonl::MAX_RPC_LINE_CHARACTERS)
            .expect("positive line limit");
        let mut buffer = [0; 8192];
        loop {
            let size = input.read(&mut buffer).await?;
            let records = if size == 0 {
                lines.finish()
            } else {
                lines.push(&buffer[..size])
            };
            for record in records {
                if let crate::jsonl::LineRecord::Line(line) = record
                    && self.observe(&line)
                {
                    changed(self);
                }
            }
            if size == 0 {
                return Ok(());
            }
        }
    }
    pub fn busy_turns(&self) -> u64 {
        self.busy.values().filter(|count| **count > 0).count() as u64
    }
    pub fn observe(&mut self, line: &str) -> bool {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        let Some(session) = event["sessionId"].as_str() else {
            return false;
        };
        match event["type"].as_str() {
            Some("agent_start") => {
                *self.busy.entry(session.into()).or_default() += 1;
                true
            }
            Some("agent_settled") => {
                let count = self.busy.entry(session.into()).or_insert(1);
                *count = count.saturating_sub(1);
                true
            }
            _ => false,
        }
    }
    pub fn activity(
        &self,
        connections: u64,
        observer: &crate::observer_link::ObserverLink,
        now: f64,
        unknown_grace_ms: f64,
    ) -> HostActivity {
        HostActivity {
            connections,
            active_turns: crate::observer_link::active_turns_for_idle_decision(
                &crate::observer_link::UnknownActivityInput {
                    healthy: observer.healthy(),
                    unhealthy_since: observer.unhealthy_since(),
                    now,
                    unknown_grace_ms,
                    observed_busy: self.busy.values().filter(|count| **count > 0).count() as u64,
                },
            ),
        }
    }
}
pub async fn wait_for_idle_exit(
    idle_exit_ms: f64,
    mut activity: tokio::sync::watch::Receiver<HostActivity>,
) -> Result<(), tokio::sync::watch::error::RecvError> {
    let mut decider = IdleExitDecider::new(idle_exit_ms);
    let started = tokio::time::Instant::now();
    let period = std::time::Duration::from_secs_f64((idle_exit_ms / 4.).clamp(20., 1000.) / 1000.);
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let state = *activity.borrow_and_update();
        if decider.update(
            state.connections,
            state.active_turns,
            started.elapsed().as_secs_f64() * 1000.,
        ) == IdleExitDecision::Exit
        {
            return Ok(());
        }
        tokio::select! {
            ()=async{ticker.tick().await;}=>{},
            changed=activity.changed()=>changed?,
        }
    }
}
#[cfg(unix)]
pub async fn proxy_connection(
    mut client: tokio::net::UnixStream,
    internal_socket: &std::path::Path,
    internal_secret: Option<&[u8]>,
) -> std::io::Result<()> {
    let mut internal = tokio::net::UnixStream::connect(internal_socket).await?;
    if let Some(secret) = internal_secret {
        crate::socket_transport::send_socket_handshake(&mut internal, secret).await?;
    }
    tokio::io::copy_bidirectional(&mut client, &mut internal).await?;
    Ok(())
}
/// One authenticated public client proxied to the internal named pipe behind the internal secret.
#[cfg(windows)]
pub async fn proxy_connection(
    mut client: tokio::net::windows::named_pipe::NamedPipeServer,
    internal_address: &str,
    internal_secret: Option<&[u8]>,
) -> std::io::Result<()> {
    let mut internal =
        win32_pipe::connect(internal_address, std::time::Duration::from_secs(5)).await?;
    if let Some(secret) = internal_secret {
        crate::socket_transport::send_socket_handshake(&mut internal, secret).await?;
    }
    tokio::io::copy_bidirectional(&mut client, &mut internal).await?;
    Ok(())
}
#[cfg(not(any(unix, windows)))]
pub async fn proxy_connection(
    _client: (),
    _internal_socket: &std::path::Path,
    _internal_secret: Option<&[u8]>,
) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "rpc host sockets are not supported on this platform yet",
    ))
}
/// `run_socket_proxy` for win32: the public and internal endpoints are named-pipe addresses.
#[cfg(windows)]
pub async fn run_socket_proxy(
    listener: tokio::net::windows::named_pipe::NamedPipeServer,
    public_address: String,
    internal_address: String,
    public_secret: Option<Vec<u8>>,
    internal_secret: Option<Vec<u8>>,
    draining: tokio::sync::watch::Receiver<bool>,
    connections: tokio::sync::watch::Sender<usize>,
) -> std::io::Result<()> {
    win32_pipe::run_pipe_proxy(
        listener,
        public_address,
        internal_address,
        public_secret,
        internal_secret,
        draining,
        connections,
    )
    .await
}
#[cfg(not(any(unix, windows)))]
pub async fn run_socket_proxy(
    _listener: (),
    _internal_socket: std::path::PathBuf,
    _public_secret: Option<Vec<u8>>,
    _internal_secret: Option<Vec<u8>>,
    _draining: tokio::sync::watch::Receiver<bool>,
    _connections: tokio::sync::watch::Sender<usize>,
) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "rpc host sockets are not supported on this platform yet",
    ))
}
#[cfg(unix)]
pub async fn run_socket_proxy(
    listener: tokio::net::UnixListener,
    internal_socket: std::path::PathBuf,
    public_secret: Option<Vec<u8>>,
    internal_secret: Option<Vec<u8>>,
    mut draining: tokio::sync::watch::Receiver<bool>,
    connections: tokio::sync::watch::Sender<usize>,
) -> std::io::Result<()> {
    let mut clients = tokio::task::JoinSet::new();
    loop {
        if *draining.borrow() {
            break;
        }
        tokio::select! {
            biased;
            changed=draining.changed()=>{if changed.is_err()||*draining.borrow(){break;}},
            result=clients.join_next(),if !clients.is_empty()=>{let _=result;connections.send_replace(clients.len());},
            accepted=listener.accept()=>{
                let(mut client,_)=accepted?;
                if *draining.borrow(){break;}
                let path=internal_socket.clone();let public=public_secret.clone();let internal=internal_secret.clone();
                clients.spawn(async move{
                    if let Some(secret)=public{crate::socket_transport::authenticate_socket(&mut client,&secret).await?;}
                    proxy_connection(client,&path,internal.as_deref()).await
                });
                connections.send_replace(clients.len());
            }
        }
    }
    drop(listener);
    while clients.join_next().await.is_some() {
        connections.send_replace(clients.len());
    }
    Ok(())
}
#[derive(Debug, PartialEq, Eq)]
pub struct SupervisorLaunch {
    pub socket: String,
    pub host_args: Vec<String>,
    pub child_command: Option<String>,
    pub child_args: Option<Vec<String>>,
    pub agent_dir: Option<String>,
    pub bind_socket: Option<String>,
    pub replace_identity: Option<crate::socket_ownership::SocketFileIdentity>,
}
pub fn find_internal_supervisor_args(argv: &[String]) -> Option<&[String]> {
    let mut index = 0;
    while index < argv.len() {
        if argv[index] == INTERNAL_SUPERVISOR_FLAG {
            return Some(&argv[index + 1..]);
        }
        if argv[index] != "--extension" || index + 1 >= argv.len() {
            return None;
        }
        index += 2;
    }
    None
}
pub fn parse_supervisor_args(argv: &[String]) -> Option<SupervisorLaunch> {
    let mut launch = SupervisorLaunch {
        socket: String::new(),
        host_args: vec![],
        child_command: None,
        child_args: None,
        agent_dir: None,
        bind_socket: None,
        replace_identity: None,
    };
    let mut socket_seen = false;
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        if index + 1 < argv.len()
            && matches!(
                arg.as_str(),
                "--socket"
                    | "--child-command"
                    | "--child-args"
                    | "--agent-dir"
                    | "--bind"
                    | "--replace"
            )
        {
            index += 1;
            let value = &argv[index];
            match arg.as_str() {
                "--socket" => {
                    launch.socket = value.clone();
                    socket_seen = true;
                }
                "--child-command" => launch.child_command = Some(value.clone()),
                "--child-args" => {
                    let parsed: serde_json::Value = serde_json::from_str(value).ok()?;
                    launch.child_args = parsed.as_array().and_then(|items| {
                        items
                            .iter()
                            .map(|item| item.as_str().map(str::to_owned))
                            .collect()
                    });
                }
                "--agent-dir" => launch.agent_dir = Some(value.clone()),
                "--bind" => launch.bind_socket = Some(value.clone()),
                "--replace" => {
                    launch.replace_identity = value
                        .split_once(':')
                        .filter(|(dev, ino)| {
                            !dev.is_empty()
                                && !ino.is_empty()
                                && dev
                                    .bytes()
                                    .chain(ino.bytes())
                                    .all(|byte| byte.is_ascii_digit())
                        })
                        .and_then(|(dev, ino)| {
                            Some(crate::socket_ownership::SocketFileIdentity {
                                dev: dev.parse().ok()?,
                                ino: ino.parse().ok()?,
                            })
                        })
                }
                _ => unreachable!(),
            }
        } else {
            launch.host_args.push(arg.clone());
        }
        index += 1;
    }
    socket_seen.then_some(launch)
}
/**
 * The host supervisor: bind the public endpoint, run a private host behind it, proxy clients,
 * and leave when idle or when the endpoint is taken over (senpi `runHostSupervisor`).
 *
 * The supervisor owns the public secret on win32 and the internal hop's directory; the child
 * only sees the internal socket. It re-enters the committed CLI entry so the host it serves is
 * built from this same executable.
 */
#[cfg(any(unix, windows))]
pub async fn run_host_supervisor(launch: SupervisorLaunch) -> std::io::Result<()> {
    // senpi `reapOrphanedInternalHostDirs`: scratch dirs of supervisors that died without cleanup.
    crate::host_internal_dir_reaper::reap_orphaned_internal_host_dirs();
    let platform = if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    };
    let agent_dir = launch
        .agent_dir
        .clone()
        .unwrap_or_else(maho_core::config::get_agent_dir);
    let paths = crate::host_daemon_paths::create_host_daemon_paths(
        &launch.socket,
        std::path::Path::new(&agent_dir),
    );
    crate::host_daemon_paths::create_daemon_directories(&paths).map_err(|error| error.source)?;
    let env: HashMap<String, String> = std::env::vars().collect();
    let instance_id = env
        .get(crate::protocol_identity::HOST_INSTANCE_ID_ENV)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| crate::protocol_identity::resolve_instance_id(None));
    let settings = crate::host_daemon_state::read_host_settings(&paths)?;
    let policy = resolve_host_policy(
        &settings
            .map(serde_json::Value::Object)
            .unwrap_or(serde_json::Value::Null),
        &env,
    );
    let public_socket = launch.socket.clone();
    let bind_socket = launch
        .bind_socket
        .clone()
        .unwrap_or_else(|| public_socket.clone());
    // win32: the supervisor owns the public secret, provisioned BEFORE the internal hop and the child so a
    // provisioning failure leaves no scratch directory and no host process behind (senpi #1370).
    let public_secret: Option<Vec<u8>> = if platform == "win32" {
        Some(ensure_public_socket_secret(&public_socket)?.to_vec())
    } else {
        None
    };
    let internal = create_internal_socket_path(&paths.dir, platform)?;
    let internal_secret: Option<Vec<u8>> = if platform == "win32" {
        let secret_path = internal
            .secret_path
            .as_deref()
            .ok_or_else(|| std::io::Error::other("internal socket has no secret path"))?;
        Some(crate::socket_transport::create_socket_secret(secret_path)?.to_vec())
    } else {
        None
    };
    let child_launch = resolve_host_child_launch(&launch, &internal.socket)?;
    let mut child_env = crate::host_successor::successor_env(
        env.clone(),
        0,
        &instance_id,
        &paths.dir.to_string_lossy(),
        Some(&agent_dir),
        &std::collections::HashMap::new(),
    );
    // win32 names no public socket to the child: a named pipe has no filesystem entry to own.
    if platform != "win32" {
        child_env.insert(
            crate::host_watchdog::HOST_PUBLIC_SOCKET_ENV.into(),
            public_socket.clone(),
        );
    }
    if internal_secret.is_some()
        && let Some(secret_path) = &internal.secret_path
    {
        child_env.insert(
            crate::socket_transport::SOCKET_SECRET_FILE_ENV.into(),
            secret_path.to_string_lossy().into_owned(),
        );
    }
    if let Some(dir) = &internal.dir {
        child_env.insert(
            crate::host_watchdog::HOST_SCRATCH_DIR_ENV.into(),
            dir.to_string_lossy().into_owned(),
        );
    }
    // Lifetime binding: this supervisor owns the write end of a pipe the child reads, so its EOF
    // means this process died for ANY reason, including a SIGKILL that runs no handler. The ppid
    // fallback covers a descriptor that could not be inherited.
    child_env.insert(
        crate::host_watchdog::HOST_WATCH_PPID_ENV.into(),
        std::process::id().to_string(),
    );
    let mut cleanup_paths: Vec<String> = Vec::new();
    if launch.bind_socket.is_none() {
        let generation = crate::host_daemon_paths::generation_paths(&paths, &instance_id);
        cleanup_paths.push(paths.pointer_file.to_string_lossy().into_owned());
        cleanup_paths.push(generation.pid_file.to_string_lossy().into_owned());
        cleanup_paths.push(paths.settings_file.to_string_lossy().into_owned());
    }
    // Windows named pipes have no filesystem entry to own, so the public entry stays listed for the crash-path
    // cleanup (senpi `hostCrashCleanupPaths`).
    if platform == "win32" {
        cleanup_paths.push(public_socket.clone());
    }
    child_env.insert(
        crate::host_watchdog::HOST_CLEANUP_PATHS_ENV.into(),
        cleanup_paths.join("\n"),
    );
    #[cfg(unix)]
    let _watch_guard = bind_child_watch_pipe(&mut child_env)?;
    let stderr = {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&paths.stderr_log)?
    };
    let spawnable = spawnable_child_launch(&child_launch, platform);
    #[cfg(unix)]
    let mut child = tokio::process::Command::new(&spawnable.command)
        .args(&spawnable.args)
        .env_clear()
        .envs(child_env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(stderr)
        .process_group(0)
        .spawn()?;
    // win32: no unix lifetime pipe and no process group. The supervisor is spawned detached and owns no console,
    // so CREATE_NO_WINDOW gives the child a console without a window (senpi `windowsHide`); the lifetime
    // binding is the ppid watch alone.
    #[cfg(windows)]
    let mut child = {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = if spawnable.shell {
            // Node's `shell: true`: `cmd.exe /d /s /c "<command> <args>"`, every piece pre-quoted verbatim.
            let mut command = tokio::process::Command::new("cmd.exe");
            command.raw_arg("/d /s /c \"").raw_arg(&spawnable.command);
            for arg in &spawnable.args {
                command.raw_arg(" ").raw_arg(arg);
            }
            command.raw_arg("\"");
            command
        } else {
            let mut command = tokio::process::Command::new(&spawnable.command);
            command.args(&spawnable.args);
            command
        };
        command
            .env_clear()
            .envs(child_env)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(stderr)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?
    };
    let child_pid = child
        .id()
        .ok_or_else(|| std::io::Error::other("supervised host has no process identity"))?;
    let child_started_at = chrono::Utc::now().timestamp_millis();
    let generation_files = crate::host_daemon_paths::generation_paths(&paths, &instance_id);
    let child_running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let child_pid_task = tokio::spawn({
        let running = child_running.clone();
        let file = generation_files.child_pid_file.clone();
        async move {
            record_child_pid(&file, child_pid, || {
                running.load(std::sync::atomic::Ordering::SeqCst)
            })
            .await;
        }
    });
    // Readiness: the internal hop must answer before the public name is published, or a client
    // that connects during startup would be proxied to nothing.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(30_000);
    let started = loop {
        if let Some(protocol) = crate::host_probe::probe_protocol_info_with_secret(
            &internal.socket,
            internal_secret.as_deref(),
            2000,
        )
        .await
            && protocol.instance_id.as_deref() == Some(instance_id.as_str())
        {
            break protocol;
        }
        if child.try_wait()?.is_some() {
            return Err(std::io::Error::other("supervised host exited before ready"));
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill().await;
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "supervised host did not become ready",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let _ = started;
    crate::host_daemon_registration::write_host_registration(
        &paths,
        &crate::host_daemon_registration::HostRegistration {
            pid: child_pid,
            process_start_time: crate::host_reservations::read_process_start_time(child_pid),
            socket: public_socket.clone(),
            instance_id: instance_id.clone(),
            generation: 0.,
            launch_profile_id: String::new(),
        },
    )
    .map_err(|error| error.source)?;
    // The signal-free drain request (senpi#3653): armed BEFORE it is advertised, so an asker that finds the marker
    // never writes into a generation that is not yet reading.
    let drain_relay = DrainRelay {
        file: generation_files.child_drain_request_file.clone(),
        sender: crate::host_stop_intent::HostStopSender {
            pid: std::process::id(),
            kind: "supervisor".into(),
            generation: Some(instance_id.clone()),
        },
    };
    let (drain_tx, mut drain_rx) = tokio::sync::watch::channel(false);
    let drain_request_watch = accept_drain_requests(&generation_files, drain_tx.clone());
    // Publish the public endpoint: an ordinary start binds the public name directly; a successor
    // binds its own generation name and adopts the public one by rename.
    #[cfg(unix)]
    let listener = {
        let listener = tokio::net::UnixListener::bind(&bind_socket)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bind_socket, std::fs::Permissions::from_mode(0o600))?;
        }
        if launch.bind_socket.is_some() && bind_socket != public_socket {
            std::fs::rename(&bind_socket, &public_socket)?;
        }
        listener
    };
    // win32: the pipe name derives from the logical path AND the public secret; a named pipe has no entry to
    // rename, so a successor generation cannot adopt the public name (senpi `adoptPublicSocket` finds no entry).
    #[cfg(windows)]
    let (listener, public_address) = {
        if launch.bind_socket.is_some() {
            return Err(std::io::Error::other(format!(
                "{public_socket}: owned by another socket entry now; refusing to replace it"
            )));
        }
        let address = crate::socket_transport::resolve_socket_transport_address(
            &bind_socket,
            platform,
            public_secret.as_deref(),
        )
        .map_err(std::io::Error::other)?;
        (win32_pipe::create_first_instance(&address)?, address)
    };
    let bound = if launch.bind_socket.is_some() {
        crate::socket_ownership::stat_socket_identity(std::path::Path::new(&public_socket))?
    } else {
        crate::socket_ownership::stat_socket_identity(std::path::Path::new(&bind_socket))?
    };
    // Publish the public entry's ownership token in this supervisor's private directory (senpi
    // `supervisorPublicOwnerFile`). The host reads it before the watchdog removes that directory, so
    // its crash-path shutdown can unlink the public socket only while it still matches this generation.
    if let Some(dir) = &internal.dir
        && let Some(identity) = bound
    {
        let _ = crate::socket_ownership::write_socket_identity_file(
            &dir.join(crate::socket_ownership::PUBLIC_SOCKET_IDENTITY_FILE),
            identity,
        );
    }
    // Observer: the supervisor cannot see turns directly - it proxies the public socket - so the
    // idle decision reads them from one always-on connection to the internal hop.
    let turns = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (activity_tx, activity_rx) = tokio::sync::watch::channel(HostActivity::default());
    let (draining_tx, draining_rx) = tokio::sync::watch::channel(false);
    let (connections_tx, connections_rx) = tokio::sync::watch::channel(0usize);
    // Drain (senpi `drainForHandoff`/`registerSupervisorSignals`): SIGUSR1 asks the child to park
    // its sessions and leave; the supervisor keeps serving the public path until the child exits,
    // which is what ends this generation. The grace timer only re-asks - it never kills a busy child.
    #[cfg(unix)]
    let mut shutdown_rx = register_supervisor_signals(drain_tx.clone())?;
    #[cfg(windows)]
    let mut shutdown_rx = register_supervisor_signals()?;
    // win32: a child whose identity changed or vanished has exited even when no exit event came.
    let (gone_tx, mut gone_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    #[cfg(windows)]
    let identity_watch = watch_win32_child_identity(
        child_pid,
        || false,
        move || {
            let _ = gone_tx.send(());
        },
    );
    #[cfg(not(windows))]
    drop(gone_tx);
    // Losing the public entry IS a drain request (senpi `watchForSupersession`, #1893/#1961).
    let supersession = crate::host_supersession::watch_for_supersession_with_pointer(
        public_socket.clone(),
        bound,
        Some(crate::host_supersession::SupersessionPointer {
            file: paths.pointer_file.clone(),
            instance_id: instance_id.clone(),
        }),
        || false,
        {
            let drain_tx = drain_tx.clone();
            move |_loss| {
                let _ = drain_tx.send(true);
            }
        },
    );
    let observer_busy = turns.clone();
    let observer_socket = internal.socket.clone();
    let observer_draining = draining_rx.clone();
    #[cfg(windows)]
    let observer_secret = internal_secret.clone();
    let observer = tokio::spawn(async move {
        let mut observed = ObservedHostTurns::default();
        loop {
            if *observer_draining.borrow() {
                return;
            }
            #[cfg(unix)]
            let connected = tokio::net::UnixStream::connect(&observer_socket).await.ok();
            #[cfg(windows)]
            let connected = async {
                let mut stream =
                    win32_pipe::connect(&observer_socket, std::time::Duration::from_secs(2))
                        .await
                        .ok()?;
                if let Some(secret) = &observer_secret {
                    crate::socket_transport::send_socket_handshake(&mut stream, secret)
                        .await
                        .ok()?;
                }
                Some(stream)
            }
            .await;
            if let Some(mut stream) = connected {
                let busy = observer_busy.clone();
                let _ = observed
                    .read_events(&mut stream, |observed| {
                        busy.store(observed.busy_turns(), std::sync::atomic::Ordering::SeqCst);
                    })
                    .await;
            }
            if *observer_draining.borrow() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });
    let activity_turns = turns.clone();
    let activity_draining = draining_rx.clone();
    let publisher = tokio::spawn(async move {
        let mut connections_rx = connections_rx;
        let mut draining_watch = activity_draining.clone();
        loop {
            if *activity_draining.borrow() {
                return;
            }
            let connections = *connections_rx.borrow_and_update() as u64;
            let active_turns = activity_turns.load(std::sync::atomic::Ordering::SeqCst);
            activity_tx.send_replace(HostActivity {
                connections,
                active_turns,
            });
            tokio::select! {
                changed=connections_rx.changed()=>{if changed.is_err(){return;}},
                ()=tokio::time::sleep(std::time::Duration::from_millis(250))=>{},
                changed=draining_watch.changed()=>{if changed.is_err()||*activity_draining.borrow(){return;}},
            }
        }
    });
    #[cfg(unix)]
    let _ = &public_secret;
    #[cfg(unix)]
    let proxy = run_socket_proxy(
        listener,
        std::path::PathBuf::from(&internal.socket),
        None,
        None,
        draining_rx.clone(),
        connections_tx,
    );
    #[cfg(windows)]
    let proxy = run_socket_proxy(
        listener,
        public_address,
        internal.socket.clone(),
        public_secret.clone(),
        internal_secret.clone(),
        draining_rx.clone(),
        connections_tx,
    );
    tokio::pin!(proxy);
    let idle = wait_for_idle_exit(policy.idle_exit_ms, activity_rx);
    tokio::pin!(idle);
    let mut draining = false;
    let mut signal_code: Option<i32> = None;
    let mut handoff_grace: Option<tokio::task::JoinHandle<()>> = None;
    // Only proxy/idle completion, the child exiting, or a shutdown signal ends the supervisor. A
    // drain starts the grace and loops again, so busy sessions keep running until the child exits.
    loop {
        tokio::select! {
            result=&mut proxy=>{result?;break;},
            result=&mut idle=>{let _=result;break;},
            status=child.wait()=>{
                // Record BEFORE shutting down (senpi `child.once("exit")` -> `noteChildExit`): a
                // crash leaves evidence in the daemon directory; a clean idle exit writes nothing.
                child_running.store(false,std::sync::atomic::Ordering::SeqCst);
                if let Ok(status)=status{
                    #[cfg(unix)]
                    let signal={use std::os::unix::process::ExitStatusExt;status.signal().map(signal_name)};
                    #[cfg(not(unix))]
                    let signal:Option<String>=None;
                    crate::host_child_exit::note_child_exit(&paths.dir,status.code(),signal.as_deref(),child_started_at,chrono::Utc::now().timestamp_millis());
                }
                break;
            },
            Some(())=gone_rx.recv()=>{
                // senpi: "rpc host child exit observed by identity watchdog", exit 0.
                child_running.store(false,std::sync::atomic::Ordering::SeqCst);
                break;
            },
            _changed=shutdown_rx.changed()=>{if let Some(code)=*shutdown_rx.borrow(){signal_code=Some(code);break;}},
            changed=drain_rx.changed()=>{
                if changed.is_ok()&&*drain_rx.borrow()&&!draining{
                    draining=true;
                    eprintln!("senpi rpc host supervisor: draining into the next generation");
                    request_child_drain(child_pid,&child_running,Some(&drain_relay),std::env::consts::OS=="windows");
                    let grace_ms=resolve_handoff_grace_ms(&std::env::vars().collect::<HashMap<String,String>>());
                    let grace_running=child_running.clone();let grace_relay=drain_relay.clone();
                    handoff_grace=Some(tokio::spawn(async move{
                        tokio::time::sleep(std::time::Duration::from_secs_f64(grace_ms/1000.)).await;
                        eprintln!("senpi rpc host supervisor: {}",serde_json::json!({"event":"handoff_grace_expired","graceMs":grace_ms}));
                        request_child_drain(child_pid,&grace_running,Some(&grace_relay),std::env::consts::OS=="windows");
                    }));
                }
            },
        }
    }
    // Teardown: stop accepting, let the host exit, then remove only the entries this generation
    // still owns so a replacement's freshly published socket survives.
    supersession.stop();
    drain_request_watch.stop();
    child_pid_task.abort();
    if let Some(task) = handoff_grace {
        task.abort();
    }
    draining_tx.send_replace(true);
    publisher.abort();
    observer.abort();
    #[cfg(windows)]
    identity_watch.abort();
    #[cfg(unix)]
    crate::host_stop::signal_generation(child_pid, rustix::process::Signal::TERM).ok();
    // win32 has no SIGTERM: Node's `process.kill(pid, "SIGTERM")` there is TerminateProcess.
    #[cfg(windows)]
    let _ = child.start_kill();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill().await;
            let _ = child.wait().await;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // Named-pipe handles can outlive cleanup; the bound (armed once the child is gone, senpi `performShutdown`)
    // guarantees the supervisor never stays resident. Inert off win32.
    let _hard_exit = ShutdownHardExit::arm(platform, signal_code.unwrap_or(0));
    crate::socket_ownership::unlink_owned_socket(&public_socket, bound, platform, |_| {});
    crate::host_daemon_registration::release_generation(&paths, &instance_id, child_pid)?;
    crate::host_daemon_registration::release_generation(&paths, &instance_id, std::process::id())?;
    if let Some(dir) = internal.dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    if let Some(code) = signal_code {
        std::process::exit(code);
    }
    Ok(())
}
#[cfg(not(any(unix, windows)))]
pub async fn run_host_supervisor(_launch: SupervisorLaunch) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "rpc host sockets are not supported on this platform yet",
    ))
}
/// Node's name for a terminating signal, as the crash record stores it (`SIGBUS`, `SIGSEGV`, ...).
#[cfg(unix)]
fn signal_name(signal: i32) -> String {
    use rustix::process::Signal;
    [
        (Signal::HUP, "SIGHUP"),
        (Signal::INT, "SIGINT"),
        (Signal::QUIT, "SIGQUIT"),
        (Signal::ILL, "SIGILL"),
        (Signal::TRAP, "SIGTRAP"),
        (Signal::ABORT, "SIGABRT"),
        (Signal::BUS, "SIGBUS"),
        (Signal::FPE, "SIGFPE"),
        (Signal::KILL, "SIGKILL"),
        (Signal::USR1, "SIGUSR1"),
        (Signal::SEGV, "SIGSEGV"),
        (Signal::USR2, "SIGUSR2"),
        (Signal::PIPE, "SIGPIPE"),
        (Signal::ALARM, "SIGALRM"),
        (Signal::TERM, "SIGTERM"),
    ]
    .into_iter()
    .find(|(known, _)| known.as_raw() == signal)
    .map_or_else(|| format!("SIG{signal}"), |(_, name)| name.to_owned())
}
#[derive(Debug, PartialEq)]
pub struct HostLifecyclePolicy {
    pub cold_start: String,
    pub idle_exit_ms: f64,
}
pub const INTERNAL_HOST_DIR_PREFIX: &str = "senpi-rpc-host-internal-";
const SHORT_TEMP_ROOT: &str = "/tmp";
/// Where the internal hop directories may live: the OS temp directory first, then `/tmp` for a temp directory too
/// deep for sun_path (a nested per-user or per-job TMPDIR, senpi#3487). The reaper scans the same roots, so a hop
/// placed in the fallback is still collected.
pub fn internal_host_dir_roots() -> Vec<std::path::PathBuf> {
    let primary = std::env::temp_dir();
    let shorter = std::path::PathBuf::from(SHORT_TEMP_ROOT);
    let canonical_primary = std::fs::canonicalize(&primary).unwrap_or_else(|_| primary.clone());
    let canonical_short = std::fs::canonicalize(&shorter).unwrap_or_else(|_| shorter.clone());
    if canonical_primary == canonical_short {
        vec![if primary.as_os_str().len() < shorter.as_os_str().len() {
            primary
        } else {
            shorter
        }]
    } else {
        vec![primary, shorter]
    }
}
fn internal_host_dir_root(name: &str) -> std::io::Result<std::path::PathBuf> {
    let roots = internal_host_dir_roots();
    for root in &roots {
        if root.join(name).join("host.sock").as_os_str().len()
            <= crate::socket_ownership::MAX_SOCKET_PATH_BYTES
        {
            return Ok(root.clone());
        }
    }
    Err(std::io::Error::other(format!(
        "internal_socket_path_too_long: {} exceeds {} bytes and /tmp cannot hold it either; set TMPDIR to a shorter directory",
        std::env::temp_dir().join(name).join("host.sock").display(),
        crate::socket_ownership::MAX_SOCKET_PATH_BYTES
    )))
}
/// The private directory a supervisor's internal hop lives in (senpi `createInternalSocketPath`).
#[derive(Debug, Clone, PartialEq)]
pub struct InternalSocketPath {
    pub socket: String,
    pub dir: Option<std::path::PathBuf>,
    pub secret_path: Option<std::path::PathBuf>,
}
fn random_short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()
}
/// A supervisor's own internal socket: private against other local users, and independently
/// named so it can never collide with the public one it proxies.
pub fn create_internal_socket_path(
    base_dir: &std::path::Path,
    platform: &str,
) -> std::io::Result<InternalSocketPath> {
    if platform == "win32" {
        let dir = base_dir.join(format!("internal-{}", random_short_id()));
        std::fs::DirBuilder::new().recursive(true).create(&dir)?;
        crate::host_daemon_paths::create_private_directory(&dir)?;
        return Ok(InternalSocketPath {
            socket: format!("\\\\.\\pipe\\senpi-rpc-internal-{}", random_short_id()),
            dir: Some(dir.clone()),
            secret_path: Some(dir.join("secret")),
        });
    }
    let name = format!("{INTERNAL_HOST_DIR_PREFIX}{}", random_short_id());
    let dir = internal_host_dir_root(&name)?.join(&name);
    crate::host_daemon_paths::create_private_directory(&dir)?;
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64);
    let owner = serde_json::json!({"pid":std::process::id(),"processStartTime":crate::host_reservations::read_process_start_time(std::process::id()),"createdAt":created_at});
    crate::host_daemon_state::write_state_file(&dir.join(".owner"), &owner)
        .map_err(|error| error.source)?;
    Ok(InternalSocketPath {
        socket: dir.join("host.sock").to_string_lossy().into_owned(),
        dir: Some(dir.clone()),
        secret_path: Some(dir.join(".secret")),
    })
}
/// The child's drain-request relay: the one way to reach a child on every platform (senpi `SupervisorDrain` relay).
#[derive(Debug, Clone)]
pub struct DrainRelay {
    pub file: std::path::PathBuf,
    pub sender: crate::host_stop_intent::HostStopSender,
}
/// Asks the child to drain: the relay file everywhere, plus SIGUSR1 as the immediate wake on POSIX (senpi
/// `requestChildDrain`). A child that already exited is never asked.
#[cfg(any(unix, windows))]
fn request_child_drain(
    child_pid: u32,
    running: &std::sync::atomic::AtomicBool,
    relay: Option<&DrainRelay>,
    windows: bool,
) {
    if !running.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Some(relay) = relay
        && let Err(error) = crate::host_stop_intent::write_drain_request(
            &relay.file,
            &relay.sender,
            "supervisor_drain",
        )
    {
        eprintln!("senpi rpc host supervisor: could not relay the drain to the host: {error}");
    }
    if windows {
        return;
    }
    #[cfg(unix)]
    if let Err(error) =
        crate::host_stop::signal_generation(child_pid, rustix::process::Signal::USR1)
    {
        eprintln!("senpi rpc host supervisor: could not ask the host to drain: {error}");
    }
    #[cfg(not(unix))]
    let _ = child_pid;
}
/// The signal-free drain request: the watcher is armed BEFORE the marker is advertised. A marker that cannot be
/// written only leaves this generation reachable by the POSIX signal, as before.
#[cfg(any(unix, windows))]
fn accept_drain_requests(
    generation: &crate::host_daemon_paths::HostGenerationPaths,
    drain_tx: tokio::sync::watch::Sender<bool>,
) -> crate::host_stop_intent::DrainRequestWatch {
    let watch = crate::host_stop_intent::watch_drain_request(
        generation.drain_request_file.clone(),
        move |request| {
            eprintln!(
                "senpi rpc host supervisor: drain requested ({})",
                request.reason
            );
            let _ = drain_tx.send(true);
        },
        None,
    );
    if let Err(error) =
        crate::host_stop_intent::write_drain_watch(&generation.drain_watch_file, std::process::id())
    {
        eprintln!("senpi rpc host supervisor: cannot advertise the drain request: {error}");
    }
    watch
}
const CHILD_START_TIME_WAIT_MS: u64 = 10_000;
fn write_child_pid_record(
    file: &std::path::Path,
    record: &serde_json::Value,
) -> std::io::Result<()> {
    use std::io::Write;
    let temporary = std::path::PathBuf::from(format!(
        "{}.{}.{}.tmp",
        file.display(),
        std::process::id(),
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        writeln!(options.open(&temporary)?, "{record}")?;
        std::fs::rename(&temporary, file)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
/// The pid is recorded at once and its start time added when it can be read: a bounded wait of 1 s probes, then
/// one unhurried read (#3658). An identity read after the child exited is never recorded: its pid may already
/// name an unrelated process (senpi `recordChildPid`). Best-effort, 0600, by rename; released with the generation.
pub async fn record_child_pid(file: &std::path::Path, pid: u32, running: impl Fn() -> bool) {
    if let Err(error) = write_child_pid_record(
        file,
        &serde_json::json!({"pid":pid,"processStartTime":null}),
    ) {
        eprintln!("senpi rpc host supervisor: could not record the host child pid: {error}");
        return;
    }
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_millis(CHILD_START_TIME_WAIT_MS);
    let mut start_time = crate::host_reservations::read_process_start_time(pid);
    while start_time.is_none() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        start_time = crate::host_reservations::read_process_start_time(pid);
    }
    // The final unhurried read of senpi (15 s CIM budget) is the same in-process read here.
    let Some(start_time) =
        start_time.or_else(|| crate::host_reservations::read_process_start_time(pid))
    else {
        eprintln!(
            "senpi rpc host supervisor: host child pid {pid} start time unreadable at recording"
        );
        return;
    };
    if !running() {
        return;
    }
    if let Err(error) = write_child_pid_record(
        file,
        &serde_json::json!({"pid":pid,"processStartTime":start_time}),
    ) {
        eprintln!("senpi rpc host supervisor: could not record the host child pid: {error}");
    }
}
/// The committed CLI entry this supervisor wraps; the native binary is its own entry.
pub fn resolve_cli_main_path() -> std::io::Result<std::path::PathBuf> {
    std::env::current_exe()
}
/// The host child spawn: an explicit child command is forwarded untouched, otherwise the
/// supervisor re-enters the committed CLI entry with the RPC multi-session flags.
pub fn resolve_host_child_launch(
    launch: &SupervisorLaunch,
    internal_socket: &str,
) -> std::io::Result<crate::host_launch::HostLaunch> {
    if let Some(command) = &launch.child_command {
        return Ok(crate::host_launch::HostLaunch {
            command: std::path::PathBuf::from(command),
            args: launch
                .child_args
                .clone()
                .unwrap_or_default()
                .into_iter()
                .chain(["--listen".to_owned(), format!("unix://{internal_socket}")])
                .collect(),
        });
    }
    Ok(crate::host_launch::HostLaunch {
        command: resolve_cli_main_path()?,
        args: ["--mode", "rpc", "--multi-session", "--listen"]
            .into_iter()
            .map(str::to_owned)
            .chain([format!("unix://{internal_socket}")])
            .chain(launch.host_args.iter().cloned())
            .collect(),
    })
}
/// A spawn shape that survives a Windows shell (senpi `spawnableChildLaunch`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnableLaunch {
    pub command: String,
    pub args: Vec<String>,
    pub shell: bool,
}
fn quote_windows_shell_arg(value: &str) -> String {
    let mut escaped = String::new();
    let mut backslashes = 0usize;
    for character in value.chars() {
        match character {
            '\\' => {
                backslashes += 1;
                escaped.push('\\');
            }
            '"' => {
                for _ in 0..=backslashes {
                    escaped.push('\\');
                }
                backslashes = 0;
                escaped.push('"');
            }
            '(' | ')' | '%' | '!' | '^' | '<' | '>' | '&' | '|' | ';' | ',' => {
                escaped.push('^');
                escaped.push(character);
            }
            _ => {
                backslashes = 0;
                escaped.push(character);
            }
        }
    }
    format!("\"{escaped}\"")
}
pub fn spawnable_child_launch(
    launch: &crate::host_launch::HostLaunch,
    platform: &str,
) -> SpawnableLaunch {
    let command = launch.command.to_string_lossy().into_owned();
    let extension = launch
        .command
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if platform != "win32" || (extension != ".cmd" && extension != ".bat") {
        return SpawnableLaunch {
            command,
            args: launch.args.clone(),
            shell: false,
        };
    }
    SpawnableLaunch {
        command: quote_windows_shell_arg(&command),
        args: launch
            .args
            .iter()
            .map(|arg| quote_windows_shell_arg(arg))
            .collect(),
        shell: true,
    }
}
pub fn parse_cold_start(value: Option<&str>) -> Option<&str> {
    value.filter(|value| matches!(*value, "transient" | "persistent"))
}
pub fn parse_idle_exit_ms(value: Option<&str>) -> Option<f64> {
    let value = value?.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite() && *number > 0.)
}
/// The soft handoff deadline the supervisor owns from the moment it drains (senpi
/// `HANDOFF_GRACE_MS_ENV` over `DEFAULT_HANDOFF_GRACE_MS`): expiry re-asks the child to drain and
/// never kills a busy one.
pub fn resolve_handoff_grace_ms(env: &HashMap<String, String>) -> f64 {
    parse_idle_exit_ms(env.get(HANDOFF_GRACE_MS_ENV).map(String::as_str))
        .unwrap_or(DEFAULT_HANDOFF_GRACE_MS)
}
/// Installs the supervisor's process signals (senpi `registerSupervisorSignals`): SIGTERM/SIGHUP
/// shut the supervisor down (143/129) and SIGUSR1 drains this generation into its successor. The
/// returned receiver carries the shutdown code; the drain trigger is the caller's channel. POSIX
/// only: win32 has no gentle signal, so it is never wired.
#[cfg(unix)]
fn register_supervisor_signals(
    drain_tx: tokio::sync::watch::Sender<bool>,
) -> std::io::Result<tokio::sync::watch::Receiver<Option<i32>>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut user1 = signal(SignalKind::user_defined1())?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel::<Option<i32>>(None);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _=terminate.recv()=>{if shutdown_tx.send(Some(143)).is_err(){break;}},
                _=hangup.recv()=>{if shutdown_tx.send(Some(129)).is_err(){break;}},
                _=user1.recv()=>{if drain_tx.send(true).is_err(){break;}},
                else=>break,
            }
        }
    });
    Ok(shutdown_rx)
}
/// Reuses a valid public secret - including one `ensureHost` just wrote - and creates one when missing or
/// unusable (senpi `ensurePublicSocketSecret`). Reuse is required: on win32 the pipe name derives from the socket
/// path AND the secret, so rotating it would move this supervisor off the endpoint its caller published.
pub fn ensure_public_socket_secret(
    public_socket: &str,
) -> std::io::Result<[u8; crate::socket_transport::SOCKET_SECRET_BYTES]> {
    let path = crate::socket_transport::socket_secret_path(public_socket)
        .map_err(std::io::Error::other)?;
    crate::socket_transport::ensure_socket_secret(std::path::Path::new(&path)).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "senpi rpc host supervisor: cannot provision public socket secret {path}: {error}"
            ),
        )
    })
}
/// win32 has no SIGTERM/SIGHUP; the closest gentle shutdown requests are the console break, close and system
/// shutdown events. Each carries senpi's SIGTERM exit code 143. There is no SIGUSR1 equivalent: a drain
/// reaches the supervisor only through the relay file.
#[cfg(windows)]
fn register_supervisor_signals() -> std::io::Result<tokio::sync::watch::Receiver<Option<i32>>> {
    use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_shutdown};
    let mut break_event = ctrl_break()?;
    let mut close_event = ctrl_close()?;
    let mut shutdown_event = ctrl_shutdown()?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel::<Option<i32>>(None);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _=break_event.recv()=>{if shutdown_tx.send(Some(143)).is_err(){break;}},
                _=close_event.recv()=>{if shutdown_tx.send(Some(143)).is_err(){break;}},
                _=shutdown_event.recv()=>{if shutdown_tx.send(Some(143)).is_err(){break;}},
                else=>break,
            }
        }
    });
    Ok(shutdown_rx)
}
pub fn resolve_host_policy(
    settings: &serde_json::Value,
    env: &HashMap<String, String>,
) -> HostLifecyclePolicy {
    let cold_start = parse_cold_start(env.get(HOST_COLD_START_ENV).map(String::as_str))
        .or_else(|| {
            parse_cold_start(
                settings
                    .get("coldStart")
                    .and_then(serde_json::Value::as_str),
            )
        })
        .unwrap_or("transient")
        .into();
    let setting_idle = settings.get("idleExitMs").and_then(|value| {
        if value.is_string() {
            value.as_str().map(str::to_owned)
        } else if value.is_number() {
            Some(value.to_string())
        } else {
            None
        }
    });
    let idle_exit_ms = parse_idle_exit_ms(env.get(HOST_IDLE_EXIT_MS_ENV).map(String::as_str))
        .or_else(|| parse_idle_exit_ms(setting_idle.as_deref()))
        .unwrap_or(DEFAULT_HOST_IDLE_EXIT_MS);
    HostLifecyclePolicy {
        cold_start,
        idle_exit_ms,
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum IdleExitDecision {
    Active,
    Idle,
    Exit,
}
pub struct IdleExitDecider {
    pub idle_exit_ms: f64,
    idle_since: Option<f64>,
}
impl IdleExitDecider {
    pub fn new(idle_exit_ms: f64) -> Self {
        Self {
            idle_exit_ms,
            idle_since: None,
        }
    }
    pub fn update(&mut self, connections: u64, active_turns: u64, now: f64) -> IdleExitDecision {
        if connections > 0 || active_turns > 0 {
            self.idle_since = None;
            return IdleExitDecision::Active;
        }
        if self.idle_exit_ms == f64::INFINITY {
            return IdleExitDecision::Idle;
        }
        let since = *self.idle_since.get_or_insert(now);
        if now - since >= self.idle_exit_ms {
            IdleExitDecision::Exit
        } else {
            IdleExitDecision::Idle
        }
    }
}

/// Win32 named-pipe shutdown can leave supervisor handles live after close starts (senpi
/// `WINDOWS_SUPERVISOR_SHUTDOWN_HARD_EXIT_MS`).
pub const WINDOWS_SUPERVISOR_SHUTDOWN_HARD_EXIT_MS: u64 = 2_000;
/// The hard-exit bound of a win32 shutdown: cleanup that leaves named-pipe handles live must never leave the
/// supervisor resident or the host orphaned. Dropping the guard (cleanup finished in time) disarms it.
pub struct ShutdownHardExit(Option<tokio::task::JoinHandle<()>>);
impl ShutdownHardExit {
    /// Arms the bound only on win32 (`platform == "win32"`); elsewhere the guard is inert.
    pub fn arm(platform: &str, exit_code: i32) -> Self {
        Self::arm_with(
            platform,
            std::time::Duration::from_millis(WINDOWS_SUPERVISOR_SHUTDOWN_HARD_EXIT_MS),
            move || std::process::exit(exit_code),
        )
    }
    pub fn arm_with(
        platform: &str,
        after: std::time::Duration,
        on_expiry: impl FnOnce() + Send + 'static,
    ) -> Self {
        if platform != "win32" {
            return Self(None);
        }
        Self(Some(tokio::spawn(async move {
            tokio::time::sleep(after).await;
            on_expiry();
        })))
    }
    pub fn armed(&self) -> bool {
        self.0.is_some()
    }
}
impl Drop for ShutdownHardExit {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}
/// The decision core of senpi `watchWin32ChildIdentity`: a child whose identity changes or disappears has exited
/// even when no exit event came. A failed baseline read is UNKNOWN, so the watch then relies on its own
/// comparisons; an absent identity is only believed once the pid probe agrees the child is gone, and only after
/// two consecutive confirmations (a loaded runner's timed-out probe means UNKNOWN, never death).
#[derive(Debug, Default)]
pub struct ChildIdentityWatch {
    baseline: Option<String>,
    missing_identity_checks: u32,
}
impl ChildIdentityWatch {
    pub fn new(baseline: Option<String>) -> Self {
        Self {
            baseline,
            missing_identity_checks: 0,
        }
    }
    /// One observation; `true` when the child is gone.
    pub fn observe(&mut self, current: Option<&str>, pid_live: bool) -> bool {
        match current {
            None if !pid_live => self.missing_identity_checks += 1,
            Some(_) => self.missing_identity_checks = 0,
            None => {}
        }
        let identity_changed = matches!((&self.baseline, current), (Some(baseline), Some(current)) if baseline != current);
        identity_changed || self.missing_identity_checks >= 2
    }
}
/// win32 only: polls the child's start identity every 500 ms until it is gone or `shutting_down` turns true, then
/// calls `on_gone` once (senpi `watchWin32ChildIdentity`). The probe is a bounded 1 s read run off the runtime.
pub fn watch_win32_child_identity(
    pid: u32,
    shutting_down: impl Fn() -> bool + Send + 'static,
    on_gone: impl FnOnce() + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let read = |pid: u32| async move {
            tokio::time::timeout(
                std::time::Duration::from_millis(1_000),
                tokio::task::spawn_blocking(move || {
                    crate::host_reservations::read_process_start_time(pid)
                }),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
        };
        let mut watch = ChildIdentityWatch::new(read(pid).await);
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if shutting_down() {
                return;
            }
            let current = read(pid).await;
            let live = crate::host_reservations::process_is_live(pid);
            if watch.observe(current.as_deref(), live) {
                on_gone();
                return;
            }
        }
    })
}
/// The Windows named-pipe listener of the supervisor's public endpoint and its authenticated internal hop (senpi
/// `host-lifecycle-proxy.ts` `listen` on win32). The pipe name is derived from the logical socket path AND the
/// secret (`socket_transport::resolve_socket_transport_address`), and the secret handshake is the only boundary:
/// a wrong secret ends the connection before a single JSONL byte is read or answered.
#[cfg(windows)]
pub mod win32_pipe {
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
    /// `ERROR_PIPE_BUSY`: every instance of the pipe is momentarily in use; the client waits and retries.
    const ERROR_PIPE_BUSY: i32 = 231;
    /// The first instance of a pipe name; a second claimant of the same name fails instead of sharing it.
    pub fn create_first_instance(address: &str) -> std::io::Result<NamedPipeServer> {
        ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(address)
    }
    fn next_instance(address: &str) -> std::io::Result<NamedPipeServer> {
        ServerOptions::new()
            .reject_remote_clients(true)
            .create(address)
    }
    /// Connects to a named pipe, waiting out a busy pipe until `timeout`.
    pub async fn connect(
        address: &str,
        timeout: std::time::Duration,
    ) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match ClientOptions::new().open(address) {
                Ok(client) => return Ok(client),
                Err(error)
                    if error.raw_os_error() == Some(ERROR_PIPE_BUSY)
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
    /// One accepted public client: authenticated with the public secret, then proxied to the internal pipe behind
    /// the internal secret.
    async fn proxy_client(
        mut client: NamedPipeServer,
        internal_address: String,
        public_secret: Option<Vec<u8>>,
        internal_secret: Option<Vec<u8>>,
    ) -> std::io::Result<()> {
        if let Some(secret) = public_secret {
            crate::socket_transport::authenticate_socket(&mut client, &secret).await?;
        }
        super::proxy_connection(client, &internal_address, internal_secret.as_deref()).await
    }
    /// `run_socket_proxy` over a named pipe: serves `public_address` until `draining` turns true, keeps every
    /// attached connection alive through the drain, and returns once they have all ended.
    pub async fn run_pipe_proxy(
        mut listener: NamedPipeServer,
        public_address: String,
        internal_address: String,
        public_secret: Option<Vec<u8>>,
        internal_secret: Option<Vec<u8>>,
        mut draining: tokio::sync::watch::Receiver<bool>,
        connections: tokio::sync::watch::Sender<usize>,
    ) -> std::io::Result<()> {
        let mut clients = tokio::task::JoinSet::new();
        loop {
            if *draining.borrow() {
                break;
            }
            tokio::select! {
                biased;
                changed = draining.changed() => { if changed.is_err() || *draining.borrow() { break; } },
                result = clients.join_next(), if !clients.is_empty() => {
                    let _ = result;
                    connections.send_replace(clients.len());
                },
                connected = listener.connect() => {
                    connected?;
                    let accepted = std::mem::replace(&mut listener, next_instance(&public_address)?);
                    if *draining.borrow() { break; }
                    clients.spawn(proxy_client(
                        accepted,
                        internal_address.clone(),
                        public_secret.clone(),
                        internal_secret.clone(),
                    ));
                    connections.send_replace(clients.len());
                },
            }
        }
        drop(listener);
        while clients.join_next().await.is_some() {
            connections.send_replace(clients.len());
        }
        Ok(())
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        fn pipe_address(tag: &str) -> String {
            crate::socket_transport::resolve_socket_transport_address(
                &format!("C:\\maho-test\\{tag}-{}.sock", std::process::id()),
                "win32",
                Some(&[3; 32]),
            )
            .unwrap_or_default()
        }
        #[tokio::test]
        async fn a_wrong_secret_is_refused_before_any_jsonl_byte_and_the_right_one_is_proxied() {
            let public = pipe_address("public");
            let internal = pipe_address("internal");
            let backend = create_first_instance(&internal).unwrap();
            let listener = create_first_instance(&public).unwrap();
            let (drain, draining) = tokio::sync::watch::channel(false);
            let (count, _counts) = tokio::sync::watch::channel(0usize);
            let proxy = tokio::spawn(run_pipe_proxy(
                listener,
                public.clone(),
                internal.clone(),
                Some(vec![1; 32]),
                Some(vec![2; 32]),
                draining,
                count,
            ));
            let scenario = async {
                let mut refused = connect(&public, std::time::Duration::from_secs(2))
                    .await
                    .unwrap();
                crate::socket_transport::send_socket_handshake(&mut refused, &[9; 32])
                    .await
                    .unwrap();
                let _ = refused
                    .write_all(b"{\"type\":\"get_protocol_info\"}\n")
                    .await;
                let mut bytes = Vec::new();
                let _ = refused.read_to_end(&mut bytes).await;
                assert!(bytes.is_empty());

                let mut client = connect(&public, std::time::Duration::from_secs(2))
                    .await
                    .unwrap();
                crate::socket_transport::send_socket_handshake(&mut client, &[1; 32])
                    .await
                    .unwrap();
                backend.connect().await.unwrap();
                let mut host = backend;
                crate::socket_transport::authenticate_socket(&mut host, &[2; 32])
                    .await
                    .unwrap();
                client.write_all(b"request\n").await.unwrap();
                let mut request = [0u8; 8];
                host.read_exact(&mut request).await.unwrap();
                assert_eq!(&request, b"request\n");
                drop(client);
                drop(host);
                drain.send(true).unwrap();
                proxy.await.unwrap().unwrap();
            };
            tokio::time::timeout(std::time::Duration::from_secs(10), scenario)
                .await
                .unwrap();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_watch_needs_two_confirmed_absences_and_never_believes_a_live_pid() {
        let mut watch = ChildIdentityWatch::new(Some("100".into()));
        assert!(!watch.observe(Some("100"), true));
        assert!(!watch.observe(None, true));
        assert!(!watch.observe(None, false));
        // A readable identity resets the count.
        assert!(!watch.observe(Some("100"), true));
        assert!(!watch.observe(None, false));
        assert!(watch.observe(None, false));
    }
    #[test]
    fn identity_watch_reports_a_changed_identity_and_works_without_a_baseline() {
        let mut watch = ChildIdentityWatch::new(Some("100".into()));
        assert!(watch.observe(Some("200"), true));
        let mut unknown = ChildIdentityWatch::new(None);
        assert!(!unknown.observe(Some("200"), true));
        assert!(!unknown.observe(None, false));
        assert!(unknown.observe(None, false));
    }
    #[tokio::test(start_paused = true)]
    async fn the_hard_exit_bound_fires_only_on_win32_and_is_disarmed_by_dropping_it() {
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = |fired: &std::sync::Arc<std::sync::atomic::AtomicUsize>| {
            let fired = fired.clone();
            move || {
                fired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        };
        let bound = std::time::Duration::from_millis(WINDOWS_SUPERVISOR_SHUTDOWN_HARD_EXIT_MS);
        assert!(!ShutdownHardExit::arm_with("linux", bound, counter(&fired)).armed());
        let disarmed = ShutdownHardExit::arm_with("win32", bound, counter(&fired));
        assert!(disarmed.armed());
        drop(disarmed);
        let armed = ShutdownHardExit::arm_with("win32", bound, counter(&fired));
        tokio::time::sleep(bound + std::time::Duration::from_millis(1)).await;
        assert_eq!(fired.load(std::sync::atomic::Ordering::SeqCst), 1);
        drop(armed);
    }
    #[test]
    fn supervisor_route_skips_only_complete_injected_prefix() {
        let args = [
            "--extension",
            "dir",
            INTERNAL_SUPERVISOR_FLAG,
            "--socket",
            "sock",
        ]
        .map(str::to_owned);
        assert_eq!(find_internal_supervisor_args(&args), Some(&args[3..]));
        assert!(
            find_internal_supervisor_args(&["--model".into(), INTERNAL_SUPERVISOR_FLAG.into()])
                .is_none()
        );
        assert!(
            find_internal_supervisor_args(&["--extension".into(), INTERNAL_SUPERVISOR_FLAG.into()])
                .is_none()
        );
    }
    #[test]
    fn supervisor_parser_keeps_forwarded_args_and_replacement_identity() {
        let args = [
            "--socket",
            "sock",
            "--replace",
            "1:2",
            "--child-args",
            "[\"one\"]",
            "--mode",
            "rpc",
        ]
        .map(str::to_owned);
        let launch = parse_supervisor_args(&args).unwrap();
        assert_eq!(launch.child_args, Some(vec!["one".into()]));
        assert_eq!(launch.host_args, vec!["--mode", "rpc"]);
        assert_eq!(
            launch.replace_identity,
            Some(crate::socket_ownership::SocketFileIdentity { dev: 1, ino: 2 })
        );
        assert!(
            parse_supervisor_args(&[
                "--socket".into(),
                "sock".into(),
                "--child-args".into(),
                "{".into()
            ])
            .is_none()
        );
    }
    #[test]
    fn continuous_idle_resets_on_activity() {
        let mut decider = IdleExitDecider::new(100.);
        assert_eq!(decider.update(0, 0, 0.), IdleExitDecision::Idle);
        assert_eq!(decider.update(0, 1, 99.), IdleExitDecision::Active);
        assert_eq!(decider.update(0, 0, 100.), IdleExitDecision::Idle);
        assert_eq!(decider.update(0, 0, 200.), IdleExitDecision::Exit);
    }
    #[test]
    fn policy_precedence_and_invalid_fallback() {
        let settings = serde_json::json!({"coldStart":"persistent","idleExitMs":1000});
        let env = HashMap::from([
            (HOST_COLD_START_ENV.into(), "invalid".into()),
            (HOST_IDLE_EXIT_MS_ENV.into(), " 2000 ".into()),
        ]);
        assert_eq!(
            resolve_host_policy(&settings, &env),
            HostLifecyclePolicy {
                cold_start: "persistent".into(),
                idle_exit_ms: 2000.
            }
        );
        assert!(parse_idle_exit_ms(Some("1e3")).is_none());
    }
    #[test]
    fn internal_host_dir_roots_start_at_tmpdir_and_fall_back_to_tmp() {
        let roots = internal_host_dir_roots();
        assert_eq!(roots.first(), Some(&std::env::temp_dir()));
        assert!(roots.contains(&std::path::PathBuf::from("/tmp")));
        assert!(internal_host_dir_root("senpi-rpc-host-internal-abcd1234").is_ok());
        let long = "x".repeat(120);
        let error = internal_host_dir_root(&long).unwrap_err().to_string();
        assert!(error.starts_with("internal_socket_path_too_long: "));
        assert!(error.contains("set TMPDIR to a shorter directory"));
    }
    #[tokio::test]
    async fn child_pid_is_recorded_with_its_start_time_only_while_it_runs() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("host-child.pid");
        let pid = std::process::id();
        record_child_pid(&file, pid, || true).await;
        let record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(record["pid"], pid);
        assert_eq!(
            record["processStartTime"].as_str().map(str::to_owned),
            crate::host_reservations::read_process_start_time(pid)
        );
        let stopped = temp.path().join("stopped.pid");
        record_child_pid(&stopped, pid, || false).await;
        let record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&stopped).unwrap()).unwrap();
        assert!(record["processStartTime"].is_null());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn drain_requests_reach_the_supervisor_without_a_signal_and_are_advertised_after_arming()
    {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::host_daemon_directory_paths(temp.path());
        let generation = crate::host_daemon_paths::generation_paths(&paths, "g");
        crate::host_daemon_paths::create_generation_directory(&generation).unwrap();
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let watch = accept_drain_requests(&generation, tx);
        assert_eq!(
            crate::host_stop_intent::read_drain_watch(&generation.drain_watch_file),
            Some(std::process::id())
        );
        let sender = crate::host_stop_intent::HostStopSender {
            pid: 1,
            kind: "stop".into(),
            generation: None,
        };
        crate::host_stop_intent::write_drain_request(
            &generation.drain_request_file,
            &sender,
            "operator_drain",
        )
        .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            rx.wait_for(|drain| *drain),
        )
        .await
        .unwrap()
        .unwrap();
        watch.stop();
    }
    #[cfg(unix)]
    #[test]
    fn child_drain_is_relayed_by_file_and_skipped_once_the_child_exited() {
        let temp = tempfile::tempdir().unwrap();
        let relay = DrainRelay {
            file: temp.path().join("host-drain-request.json"),
            sender: crate::host_stop_intent::HostStopSender {
                pid: 1,
                kind: "supervisor".into(),
                generation: None,
            },
        };
        let gone = std::sync::atomic::AtomicBool::new(false);
        request_child_drain(0, &gone, Some(&relay), true);
        assert!(!relay.file.exists());
        let running = std::sync::atomic::AtomicBool::new(true);
        request_child_drain(0, &running, Some(&relay), true);
        assert_eq!(
            crate::host_stop_intent::read_drain_request(&relay.file)
                .unwrap()
                .reason,
            "supervisor_drain"
        );
    }
    #[test]
    fn persistent_never_idle_exits() {
        assert_eq!(
            IdleExitDecider::new(f64::INFINITY).update(0, 0, 100000000.),
            IdleExitDecision::Idle
        );
    }
    #[test]
    fn handoff_grace_prefers_the_environment_and_falls_back_to_ten_minutes() {
        assert_eq!(
            resolve_handoff_grace_ms(&HashMap::new()),
            DEFAULT_HANDOFF_GRACE_MS
        );
        assert_eq!(
            resolve_handoff_grace_ms(&HashMap::from([(
                HANDOFF_GRACE_MS_ENV.into(),
                "1000".into()
            )])),
            1000.
        );
        assert_eq!(
            resolve_handoff_grace_ms(&HashMap::from([(
                HANDOFF_GRACE_MS_ENV.into(),
                "bad".into()
            )])),
            DEFAULT_HANDOFF_GRACE_MS
        );
    }
}
