pub use crate::host_child_exit::classify_child_exit;
use std::collections::HashMap;
pub const HOST_COLD_START_ENV: &str = "SENPI_RPC_HOST_COLD_START";
pub const HOST_IDLE_EXIT_MS_ENV: &str = "SENPI_RPC_HOST_IDLE_EXIT_MS";
pub const DEFAULT_HOST_IDLE_EXIT_MS: f64 = 900000.;
pub const HANDOFF_GRACE_MS_ENV: &str = "SENPI_RPC_HANDOFF_GRACE_MS";
pub const DEFAULT_HANDOFF_GRACE_MS: f64 = 600000.;
pub use crate::host_launch_spec::HostLifecyclePolicyInput;
// The launch surface lives in its own module (senpi host-lifecycle-launch.ts); re-exported at its original home.
#[cfg(unix)]
use crate::host_lifecycle_launch::bind_child_watch_pipe;
pub use crate::host_lifecycle_launch::{
    CHILD_WATCH_FD, INTERNAL_SUPERVISOR_FLAG, SpawnableLaunch, SupervisorLaunch,
    find_internal_supervisor_args, parse_supervisor_args, resolve_cli_main_path,
    resolve_host_child_launch, spawnable_child_launch,
};
#[derive(Clone, Copy, Default)]
pub struct HostActivity {
    pub connections: u64,
    pub active_turns: u64,
}
#[derive(Default)]
pub struct ObservedHostTurns {
    runs: crate::host_run_activity::SessionRunActivity,
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
        self.runs.busy_sessions() as u64
    }
    pub fn observe(&mut self, line: &str) -> bool {
        // agent_start repeats for each loop iteration, but agent_settled ends the whole run.
        // Share the owner's run semantics so idle shutdown cannot retain phantom turns.
        self.runs.observe(line)
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
                    observed_busy: self.busy_turns(),
                },
            ),
        }
    }
}
/// The observer connection's event stream: every line feeds both the idle decision's turn count and the owner
/// check's run activity (senpi `observeHostEvent`).
#[cfg(any(unix, windows))]
async fn observe_host_events(
    mut input: impl tokio::io::AsyncRead + Unpin,
    observed: &mut ObservedHostTurns,
    busy: &std::sync::atomic::AtomicU64,
    activity: &std::sync::Mutex<crate::host_lifecycle_activity::SupervisorActivity>,
) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut lines = crate::jsonl::JsonlLineReader::new(crate::jsonl::MAX_RPC_LINE_CHARACTERS)
        .map_err(std::io::Error::other)?;
    let mut buffer = [0; 8192];
    loop {
        let size = input.read(&mut buffer).await?;
        let records = if size == 0 {
            lines.finish()
        } else {
            lines.push(&buffer[..size])
        };
        for record in records {
            if let crate::jsonl::LineRecord::Line(line) = record {
                crate::host_lifecycle_activity::lock_activity(activity).observe_host_event(&line);
                if observed.observe(&line) {
                    busy.store(observed.busy_turns(), std::sync::atomic::Ordering::SeqCst);
                }
            }
        }
        if size == 0 {
            return Ok(());
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
    // The inherited owner read end is adopted close-on-exec before anything is spawned, so neither the host child
    // nor its descendants inherit it.
    let owner_pipe = launch
        .owner_fd
        .and_then(crate::host_lifecycle_activity::adopt_owner_pipe_fd);
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
    let state = std::sync::Arc::new(crate::host_lifecycle_shutdown::SupervisorState::default());
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
    // senpi `prepareInternalHop`: a Windows pipe secret, or a fresh memory-only POSIX key the child gets on stdin.
    let hop = crate::host_lifecycle_internal_key::prepare_internal_hop(&paths.dir, platform)?;
    let internal = hop.internal;
    let internal_secret = hop.internal_secret;
    let internal_key = hop.internal_key;
    // The public endpoint's per-launch key (POSIX): published only after the public bind, disposed at teardown.
    #[cfg(unix)]
    let public_endpoint = if platform == "win32" {
        None
    } else {
        Some(std::sync::Arc::new(
            crate::rpc_public_key::RpcPublicEndpoint::new(
                std::path::Path::new(&agent_dir),
                &public_socket,
            )
            .map_err(std::io::Error::other)?,
        ))
    };
    let child_launch = resolve_host_child_launch(&launch, &internal.socket)?;
    let mut cleanup_paths: Vec<std::path::PathBuf> = Vec::new();
    if launch.bind_socket.is_none() {
        let generation = crate::host_daemon_paths::generation_paths(&paths, &instance_id);
        cleanup_paths.push(paths.pointer_file.clone());
        cleanup_paths.push(generation.pid_file.clone());
        cleanup_paths.push(paths.settings_file.clone());
    }
    // Windows named pipes have no filesystem entry to own, so the public entry stays listed for the crash-path
    // cleanup (senpi `hostCrashCleanupPaths`).
    if platform == "win32" {
        cleanup_paths.push(public_socket.clone().into());
    }
    // Linux: supervisor death reaches the child as FIFO EOF; the held writer is never closed by this process.
    let _lifetime_binding = hop.lifetime_binding;
    let daemon_dir = paths.dir.to_string_lossy().into_owned();
    // senpi `spawnHostChild`: the environment that binds the child to this supervisor and its generation.
    let child_env = crate::host_lifecycle_launch::host_child_env(
        env.clone(),
        &crate::host_lifecycle_launch::HostChildEnv {
            platform,
            instance_id: &instance_id,
            daemon_dir: &daemon_dir,
            agent_dir: &agent_dir,
            public_socket: &public_socket,
            internal: &internal,
            internal_secret: internal_secret.is_some(),
            internal_key: internal_key.is_some(),
            supervisor_start: hop.supervisor_start.as_ref(),
            lifetime_fifo: _lifetime_binding
                .as_ref()
                .map(|binding| binding.lifetime_fifo.as_path()),
            cleanup_paths: &cleanup_paths,
        },
    );
    #[cfg(unix)]
    let mut child_env = child_env;
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
    let mut spawnable = spawnable;
    // senpi `spawnHostChild`: a shell wrapper cannot prove preservation of the private inherited handle.
    // The inherited fd 4 is adopted (close-on-exec) before anything is spawned; a shell launch just closes it.
    #[cfg(unix)]
    let presenter = match launch
        .presenter_bootstrap_fd
        .and_then(crate::mcp_presenter_bootstrap::adopt_presenter_bootstrap_fd)
    {
        Some(adopted) if !spawnable.shell => {
            let channel = crate::mcp_presenter_bootstrap::ChildBootstrap::new()?;
            spawnable.args.extend([
                crate::mcp_presenter_bootstrap::MCP_PRESENTER_FD_FLAG.to_owned(),
                "4".to_owned(),
            ]);
            Some((adopted, channel))
        }
        _ => None,
    };
    #[cfg(unix)]
    let mut child = {
        let mut command = match &presenter {
            Some((_, channel)) => {
                let mut command = channel.command(std::ffi::OsStr::new(&spawnable.command));
                command.args(&spawnable.args);
                command
            }
            None => {
                let mut command = tokio::process::Command::new(&spawnable.command);
                command.args(&spawnable.args);
                command
            }
        };
        command
            .env_clear()
            .envs(child_env)
            // POSIX stdin is the key pipe: one write, and its write end stays alive until this supervisor exits.
            // The presenter `/bin/sh` wrapper execs the target in place, so stdin reaches it unchanged.
            .stdin(if internal_key.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::null())
            .stderr(stderr)
            .process_group(0)
            .spawn()?
    };
    #[cfg(unix)]
    let _key_pipe = match (internal_key, child.stdin.take()) {
        (Some(key), Some(mut stdin)) => {
            use tokio::io::AsyncWriteExt;
            // A write failure is the child's to report (internal_key_unavailable); it must not crash the supervisor.
            let _ = stdin.write_all(&key).await;
            Some(stdin)
        }
        (_, stdin) => stdin,
    };
    // The opt-in relay runs beside the supervisor; the child's copy of the channel was handed over at spawn.
    #[cfg(unix)]
    let _presenter_relay = presenter.and_then(|(adopted, channel)| {
        let crate::mcp_presenter_bootstrap::ChildBootstrap {
            parent,
            child: inherited,
        } = channel;
        drop(inherited);
        let upstream = crate::mcp_presenter_bootstrap::presenter_bootstrap_stream_from(adopted)?;
        // Bounded by the relay's 30 s deadline and either side's EOF; the supervisor does not re-launch with it.
        Some(tokio::spawn(
            crate::mcp_presenter_bootstrap::relay_presenter_bootstrap(
                upstream,
                parent,
                None,
                std::future::pending::<()>(),
            ),
        ))
    });
    // win32: no unix lifetime pipe and no process group. The supervisor is spawned detached and owns no console,
    // so CREATE_NO_WINDOW gives the child a console without a window (senpi `windowsHide`); the lifetime
    // binding is the ppid watch alone.
    #[cfg(windows)]
    let child = {
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
    let drain_relay = crate::host_lifecycle_drain::DrainRelay {
        file: generation_files.child_drain_request_file.clone(),
        sender: crate::host_stop_intent::HostStopSender {
            pid: std::process::id(),
            kind: "supervisor".into(),
            generation: Some(instance_id.clone()),
        },
    };
    let (drain_tx, mut drain_rx) = tokio::sync::watch::channel(false);
    let (draining_tx, draining_rx) = tokio::sync::watch::channel(false);
    // The public entry's identity once publication stat'ed it; release unlinks the entry only while it matches.
    let bound = std::sync::Arc::new(std::sync::Mutex::new(None));
    // senpi `createPublicProxy` admission authority: created before the teardown so every shutdown path stops it
    // first and flushes its pending refusals before the internal directory is removed.
    #[cfg(unix)]
    let admission = std::sync::Arc::new(crate::host_admission::HostAdmission::new(
        serde_json::json!({"protocolVersion":1,"serverVersion":maho_core::engine_build_identity::engine_build_identity().text,"capabilities":[],"instanceId":instance_id})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    ));
    let mut teardown = GenerationTeardown {
        child,
        child_pid,
        draining_tx,
        watchers: vec![Box::new(move || child_pid_task.abort())],
        platform,
        public_socket: public_socket.clone(),
        bound: bound.clone(),
        paths: paths.clone(),
        instance_id: instance_id.clone(),
        internal_dir: internal.dir.clone(),
        _hard_exit: None,
        #[cfg(unix)]
        public_endpoint: public_endpoint.clone(),
        #[cfg(unix)]
        admission: admission.clone(),
    };
    // Drain (senpi `drainForHandoff`/`registerSupervisorSignals`): SIGUSR1 asks the child to park
    // its sessions and leave; the supervisor keeps serving the public path until the child exits,
    // which is what ends this generation. The grace timer only re-asks - it never kills a busy child.
    // Registered before the startup handshake, not after it: the private internal directory already exists, so a
    // stop arriving during host startup must run the same cleanup instead of the default kill (senpi #4216).
    #[cfg(unix)]
    let shutdown_rx = register_supervisor_signals(drain_tx.clone());
    #[cfg(windows)]
    let shutdown_rx = register_supervisor_signals();
    let mut shutdown_rx = match shutdown_rx {
        Ok(shutdown_rx) => shutdown_rx,
        Err(cause) => {
            let exit_code = crate::host_lifecycle_shutdown::perform_shutdown(
                &mut teardown,
                &state,
                &format!("startup failed: {cause}"),
                1,
            )
            .await;
            drop(teardown);
            std::process::exit(exit_code);
        }
    };
    let readiness = {
        let internal_socket = internal.socket.clone();
        let internal_secret = internal_secret.clone();
        let instance_id = instance_id.clone();
        let paths = paths.clone();
        let public_socket = public_socket.clone();
        async move |teardown: &mut GenerationTeardown| -> std::io::Result<()> {
            // Readiness: the internal hop must answer before the public name is published, or a client
            // that connects during startup would be proxied to nothing.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(30_000);
            loop {
                #[cfg(unix)]
                {
                    if let Some(key) = &internal_key {
                        // senpi `waitForListener` with the internal key: one sealed probe per attempt.
                        let probe_ms = crate::host_lifecycle_proxy::INTERNAL_READINESS_PROBE_MS
                            .min(
                                deadline
                                    .saturating_duration_since(tokio::time::Instant::now())
                                    .as_millis()
                                    .max(1) as u64,
                            );
                        match crate::host_lifecycle_proxy::probe_internal_listener(
                            std::path::Path::new(&internal_socket),
                            key,
                            probe_ms,
                        )
                        .await
                        {
                            crate::host_lifecycle_proxy::ReadinessProbe::Ready(data) => {
                                if let Some(protocol) =
                                    crate::host_protocol_info::parse_host_protocol_info(&data)
                                    && protocol.instance_id.as_deref() == Some(instance_id.as_str())
                                {
                                    break;
                                }
                            }
                            crate::host_lifecycle_proxy::ReadinessProbe::Refused => {}
                            // The startup shutdown stops the child; this only says why.
                            crate::host_lifecycle_proxy::ReadinessProbe::Silent => {
                                return Err(std::io::Error::other(format!(
                                    "{internal_socket}: host accepted the internal hop but did not answer within {probe_ms}ms"
                                )));
                            }
                            // A host that answered but could not prove the key will not start proving it: fail now, and say why.
                            crate::host_lifecycle_proxy::ReadinessProbe::Unproven(reason) => {
                                return Err(std::io::Error::other(format!(
                                    "{internal_socket}: {reason}"
                                )));
                            }
                        }
                    }
                }
                if internal_key.is_none()
                    && let Some(protocol) = crate::host_probe::probe_protocol_info_with_secret(
                        &internal_socket,
                        internal_secret.as_deref(),
                        2000,
                    )
                    .await
                    && protocol.instance_id.as_deref() == Some(instance_id.as_str())
                {
                    break;
                }
                if teardown.child.try_wait()?.is_some() {
                    return Err(std::io::Error::other("supervised host exited before ready"));
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "supervised host did not become ready",
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            crate::host_daemon_registration::write_host_registration(
                &paths,
                &crate::host_daemon_registration::HostRegistration {
                    pid: child_pid,
                    process_start_time: crate::host_reservations::read_process_start_time(
                        child_pid,
                    ),
                    socket: public_socket,
                    instance_id,
                    generation: 0.,
                    launch_profile_id: String::new(),
                    build: Some(crate::host_daemon_registration::RegisteredBuild::this_engine()),
                },
            )
            .map_err(|error| error.source)
        }
    };
    // Startup publication (senpi `state.startupComplete`): shutdown stops the child at once but joins these writes
    // before it removes the directories they write into. Every failure here is `startup failed`.
    let publication = {
        let internal_dir = internal.dir.clone();
        let public_socket = public_socket.clone();
        let generation_files = generation_files.clone();
        let drain_tx = drain_tx.clone();
        #[cfg(windows)]
        let public_secret = public_secret.clone();
        #[cfg(unix)]
        let public_endpoint = public_endpoint.clone();
        let launch = &launch;
        let bind_socket = &bind_socket;
        let bound = bound.clone();
        async move {
            // Publish the public endpoint: an ordinary start binds the public name directly; a successor
            // binds its own generation name and adopts the public one by rename.
            #[cfg(unix)]
            let listener = {
                // senpi `prepareSocketPath` + `listen`: a dead entry is cleared, a live listener's is never touched.
                crate::host_lifecycle_proxy::prepare_socket_path(bind_socket).await?;
                let listener = crate::host_lifecycle_proxy::listen(bind_socket)?;
                // The key record names a listener that exists: published only after the bind succeeded.
                if let Some(endpoint) = &public_endpoint {
                    endpoint.publish().map_err(std::io::Error::other)?;
                }
                if launch.bind_socket.is_some() && *bind_socket != public_socket {
                    crate::socket_ownership::adopt_public_socket(
                        std::path::Path::new(bind_socket),
                        std::path::Path::new(&public_socket),
                        launch.replace_identity,
                    )?;
                }
                listener
            };
            // win32: the pipe name derives from the logical path AND the public secret; a named pipe has no entry
            // to rename, so a successor generation cannot adopt the public name (senpi `adoptPublicSocket` finds
            // no entry).
            #[cfg(windows)]
            let listener = {
                if launch.bind_socket.is_some() {
                    return Err(std::io::Error::other(format!(
                        "{public_socket}: owned by another socket entry now; refusing to replace it"
                    )));
                }
                let address = crate::socket_transport::resolve_socket_transport_address(
                    bind_socket,
                    platform,
                    public_secret.as_deref(),
                )
                .map_err(std::io::Error::other)?;
                (win32_pipe::create_first_instance(&address)?, address)
            };
            let identity = if launch.bind_socket.is_some() {
                crate::socket_ownership::stat_socket_identity(std::path::Path::new(&public_socket))?
            } else {
                crate::socket_ownership::stat_socket_identity(std::path::Path::new(bind_socket))?
            };
            *bound
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = identity;
            // Publish the public entry's ownership token in this supervisor's private directory (senpi
            // `supervisorPublicOwnerFile`). The host reads it before the watchdog removes that directory, so
            // its crash-path shutdown can unlink the public socket only while it still matches this generation.
            if let Some(dir) = &internal_dir
                && let Some(identity) = identity
            {
                let _ = crate::socket_ownership::write_socket_identity_file(
                    &dir.join(crate::socket_ownership::PUBLIC_SOCKET_IDENTITY_FILE),
                    identity,
                );
            }
            // The signal-free drain request (senpi#3653): armed BEFORE it is advertised, so an asker that finds the
            // marker never writes into a generation that is not yet reading.
            let drain_request_watch =
                crate::host_lifecycle_drain::accept_drain_requests(&generation_files, drain_tx);
            std::io::Result::Ok((listener, identity, drain_request_watch))
        }
    };
    let (listener, bound, drain_request_watch) =
        match crate::host_lifecycle_shutdown::drive_startup(
            &state,
            &mut teardown,
            &mut shutdown_rx,
            readiness,
            publication,
        )
        .await
        {
            Ok(published) => published,
            Err(exit_code) => {
                drop(teardown);
                std::process::exit(exit_code);
            }
        };
    // win32 publication answers the pipe server together with the address it serves.
    #[cfg(windows)]
    let (listener, public_address) = listener;
    crate::host_supervisor_log::write_host_ready(
        &public_socket,
        &policy.cold_start,
        policy.idle_exit_ms,
    );
    // Observer: the supervisor cannot see turns directly - it proxies the public socket - so the
    // idle decision reads them from one always-on connection to the internal hop.
    let turns = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (activity_tx, activity_rx) = tokio::sync::watch::channel(HostActivity::default());
    let (connections_tx, connections_rx) = tokio::sync::watch::channel(0usize);
    // senpi `SupervisorActivity`: the owner check reads quiescence from the same observer connection and clients the
    // idle decision does. The public proxy does not classify clients, so every live client is attached here: owner
    // quiescence still needs zero clients.
    let activity = std::sync::Arc::new(std::sync::Mutex::new(
        crate::host_lifecycle_activity::SupervisorActivity::new(
            if policy.cold_start == "persistent" {
                f64::INFINITY
            } else {
                policy.idle_exit_ms
            },
        ),
    ));
    // senpi `SupervisorOwner`: an ensured generation (or a launch carrying `--owner-fd`) binds its lifetime owner;
    // a direct launch without an owner record has none. The owner leaving a quiescent host ends this generation.
    let mut owner =
        (launch.owner_fd.is_some() || generation_files.settings_file.exists()).then(|| {
            let mut owner =
                crate::host_lifecycle_activity::SupervisorOwner::new(generation_files.dir.clone());
            owner.start(owner_pipe, owner_clock_ms());
            crate::host_lifecycle_activity::OwnerCheck::new(owner, activity.clone())
        });
    let on_activity: crate::host_lifecycle_activity::OnActivity = match &owner {
        Some(owner) => owner.on_activity(),
        None => std::sync::Arc::new(|| {}),
    };
    // win32: a child whose identity changed or vanished has exited even when no exit event came.
    let (gone_tx, mut gone_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    #[cfg(windows)]
    let identity_watch = crate::host_lifecycle_drain::watch_win32_child_identity(
        child_pid,
        || false,
        move || {
            let _ = gone_tx.send(());
        },
    );
    #[cfg(not(windows))]
    drop(gone_tx);
    // Losing the public entry IS a drain request (senpi `watchForSupersession`, #1893/#1961).
    // A replaced entry's registration belongs to the replacer; `release_generation` already leaves a pointer that
    // names another instance alone, so the replacement needs no further bookkeeping here.
    let supersession = crate::host_lifecycle_drain::drain_on_public_socket_loss(
        public_socket.clone(),
        bound,
        || false,
        {
            let drain_tx = drain_tx.clone();
            move || {
                let _ = drain_tx.send(true);
            }
        },
        || {},
        Some(crate::host_supersession::SupersessionPointer {
            file: paths.pointer_file.clone(),
            instance_id: instance_id.clone(),
        }),
    );
    let observer_busy = turns.clone();
    let observer_socket = internal.socket.clone();
    let observer_draining = draining_rx.clone();
    #[cfg(windows)]
    let observer_secret = internal_secret.clone();
    #[cfg(unix)]
    let observer_key = internal_key;
    let observer_activity = activity.clone();
    let observer_held = on_activity.clone();
    let observer = tokio::spawn(async move {
        let mut observed = ObservedHostTurns::default();
        let mut socket = 0u64;
        loop {
            if *observer_draining.borrow() {
                return;
            }
            // Sealed with the internal key and the 5 s handshake bound; a failed open drops the socket.
            #[cfg(unix)]
            let connected = crate::host_lifecycle_activity::connect_observer(
                std::path::Path::new(&observer_socket),
                observer_key.as_ref(),
            )
            .await
            .ok();
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
            if let Some(stream) = connected {
                socket += 1;
                crate::host_lifecycle_activity::lock_activity(&observer_activity)
                    .observer_opened(socket);
                let _ =
                    observe_host_events(stream, &mut observed, &observer_busy, &observer_activity)
                        .await;
                crate::host_lifecycle_activity::lock_activity(&observer_activity).observer_lost(
                    socket,
                    *observer_draining.borrow(),
                    owner_clock_ms(),
                );
                observer_held();
            }
            if *observer_draining.borrow() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });
    let activity_turns = turns.clone();
    let activity_draining = draining_rx.clone();
    let publisher_activity = activity.clone();
    let publisher_held = on_activity.clone();
    let publisher = tokio::spawn(async move {
        let mut connections_rx = connections_rx;
        let mut draining_watch = activity_draining.clone();
        loop {
            if *activity_draining.borrow() {
                return;
            }
            let connections = *connections_rx.borrow_and_update() as u64;
            let active_turns = activity_turns.load(std::sync::atomic::Ordering::SeqCst);
            crate::host_lifecycle_activity::lock_activity(&publisher_activity)
                .set_clients(connections, 0);
            activity_tx.send_replace(HostActivity {
                connections,
                active_turns,
            });
            tokio::select! {
                // Every admission publishes a new count: senpi `onAdmit` resets the owner grace even for a client
                // that is admitted and gone before the next tick.
                changed=connections_rx.changed()=>{if changed.is_err(){return;}publisher_held();},
                ()=tokio::time::sleep(std::time::Duration::from_millis(250))=>{},
                changed=draining_watch.changed()=>{if changed.is_err()||*activity_draining.borrow(){return;}},
            }
        }
    });
    #[cfg(unix)]
    let _ = &public_secret;
    // senpi `createPublicProxy`: sealed internal side, public key filter, admission authority (created above).
    #[cfg(unix)]
    let proxy = crate::host_lifecycle_proxy::run_public_proxy(
        listener,
        crate::host_lifecycle_proxy::PublicProxyOptions {
            internal_socket: std::path::PathBuf::from(&internal.socket),
            internal_key,
            public_endpoint: public_endpoint.clone(),
            require_token: crate::rpc_public_auth::PublicRpcConnection::require_token_from_env(),
            admission: admission.clone(),
        },
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
    let mut owner_gone = false;
    let mut owner_ticker = tokio::time::interval(std::time::Duration::from_millis(
        crate::host_lifecycle_ticker::tick_interval_ms(policy.idle_exit_ms),
    ));
    owner_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // senpi `SupervisorDrain`: `admission.stop()` refuses opens from the drain on (senpi `beginStopping`).
    let supervisor_drain = crate::host_lifecycle_drain::SupervisorDrain::new(
        child_pid,
        child_running.clone(),
        || false,
        {
            #[cfg(unix)]
            let admission = admission.clone();
            move || {
                #[cfg(unix)]
                admission.stop();
            }
        },
        Some(drain_relay),
        std::env::consts::OS == "windows",
    );
    // Only proxy/idle completion, the child exiting, or a shutdown signal ends the supervisor. A
    // drain starts the grace and loops again, so busy sessions keep running until the child exits.
    loop {
        tokio::select! {
            result=&mut proxy=>{result?;break;},
            result=&mut idle=>{let _=result;break;},
            status=teardown.child.wait()=>{
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
            _=owner_ticker.tick(),if owner.is_some()&&!draining=>{
                if let Some(owner)=owner.as_mut()
                    && owner.tick(owner_clock_ms()).await{owner_gone=true;break;}
            },
            changed=drain_rx.changed()=>{
                if changed.is_ok()&&*drain_rx.borrow()&&!draining{
                    draining=true;
                    // senpi `SupervisorDrain.drain`: admission stops, the child is asked, the soft grace re-asks.
                    supervisor_drain.drain();
                }
            },
        }
    }
    // Teardown (senpi `performShutdown`): stop accepting, stop the child, join startup publication, then remove
    // only the entries this generation still owns so a replacement's freshly published socket survives.
    let watchers: [Box<dyn FnOnce() + Send>; 4] = [
        Box::new(move || supersession.stop()),
        Box::new(move || drain_request_watch.stop()),
        Box::new(move || publisher.abort()),
        Box::new(move || observer.abort()),
    ];
    teardown.watchers.extend(watchers);
    teardown
        .watchers
        .push(Box::new(move || supervisor_drain.stop()));
    #[cfg(windows)]
    teardown
        .watchers
        .push(Box::new(move || identity_watch.abort()));
    if let Some(mut owner) = owner {
        owner.owner.stop();
    }
    let reason = match signal_code {
        Some(code) => crate::host_lifecycle_shutdown::signal_shutdown_reason(code),
        None if owner_gone => "owner_gone".to_owned(),
        None => "rpc host child exited".to_owned(),
    };
    let exit_code = crate::host_lifecycle_shutdown::perform_shutdown(
        &mut teardown,
        &state,
        &reason,
        signal_code.unwrap_or(0),
    )
    .await;
    drop(teardown);
    if signal_code.is_some() || exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}
/// The owner grace's monotonic clock, in milliseconds.
#[cfg(any(unix, windows))]
fn owner_clock_ms() -> f64 {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
        * 1000.
}
/// One generation's resources as [`crate::host_lifecycle_shutdown::perform_shutdown`] tears them down.
#[cfg(any(unix, windows))]
struct GenerationTeardown {
    child: tokio::process::Child,
    child_pid: u32,
    draining_tx: tokio::sync::watch::Sender<bool>,
    watchers: Vec<Box<dyn FnOnce() + Send>>,
    platform: &'static str,
    public_socket: String,
    /// The public entry's identity, filled in by startup publication once it stat'ed the bound entry.
    bound: std::sync::Arc<std::sync::Mutex<Option<crate::socket_ownership::SocketFileIdentity>>>,
    paths: crate::host_daemon_paths::HostDaemonPaths,
    instance_id: String,
    internal_dir: Option<std::path::PathBuf>,
    /// Held only for its Drop: the win32 hard-exit bound stays armed until the teardown is dropped.
    _hard_exit: Option<ShutdownHardExit>,
    #[cfg(unix)]
    public_endpoint: Option<std::sync::Arc<crate::rpc_public_key::RpcPublicEndpoint>>,
    /// The public proxy's admission authority: stopped first on every shutdown, flushed before cleanup.
    #[cfg(unix)]
    admission: std::sync::Arc<crate::host_admission::HostAdmission>,
}
#[cfg(windows)]
impl GenerationTeardown {
    /// Waits up to `bound` for the child's exit; `true` once it is observed.
    async fn wait_for_exit(&mut self, bound: std::time::Duration) -> std::io::Result<bool> {
        match tokio::time::timeout(bound, self.child.wait()).await {
            Ok(status) => status.map(|_| true),
            Err(_) => Ok(false),
        }
    }
}
#[cfg(any(unix, windows))]
#[async_trait::async_trait]
impl crate::host_lifecycle_shutdown::SupervisorTeardown for GenerationTeardown {
    fn stop_admission(&mut self) {
        // senpi `performShutdown`: pending opens are refused host_stopping from this instant.
        #[cfg(unix)]
        self.admission.stop();
        self.draining_tx.send_replace(true);
    }
    async fn stop_child(&mut self, reason: &str) -> std::io::Result<()> {
        // senpi `stopChild`: the intent on record first, SIGTERM, the stall wait for a stalled child, then SIGKILL.
        #[cfg(unix)]
        {
            let generation =
                crate::host_daemon_paths::generation_paths(&self.paths, &self.instance_id);
            crate::host_lifecycle_shutdown::stop_child(
                &mut self.child,
                &generation,
                &self.instance_id,
                reason,
            )
            .await
        }
        // win32 has no SIGTERM: Node's `process.kill(pid, "SIGTERM")` there is TerminateProcess.
        #[cfg(windows)]
        {
            let _ = reason;
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            let _ = self.child.start_kill();
            if self
                .wait_for_exit(std::time::Duration::from_secs(5))
                .await?
            {
                return Ok(());
            }
            self.child.start_kill()?;
            if self
                .wait_for_exit(std::time::Duration::from_secs(5))
                .await?
            {
                return Ok(());
            }
            Err(std::io::Error::other(format!(
                "host child pid {} exit unobserved after SIGKILL",
                self.child_pid
            )))
        }
    }
    async fn child_exit_recorded(&mut self) {
        // The exit is recorded in the supervisor loop when the child ends on its own (`note_child_exit`).
    }
    fn stop_watchers(&mut self) {
        for stop in self.watchers.drain(..) {
            stop();
        }
    }
    async fn release(&mut self, exit_code: i32) -> std::io::Result<()> {
        // Named-pipe handles can outlive cleanup; the bound (armed once the child is gone) guarantees the
        // supervisor never stays resident. Inert off win32.
        self._hard_exit = Some(ShutdownHardExit::arm(self.platform, exit_code));
        // The refusals already queued are flushed within their bound before the internal directory goes.
        #[cfg(unix)]
        self.admission.flush().await;
        if let Some(dir) = self.internal_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
        let bound = *self
            .bound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::socket_ownership::unlink_owned_socket(
            &self.public_socket,
            bound,
            self.platform,
            |_| {},
        );
        crate::host_daemon_registration::release_generation(
            &self.paths,
            &self.instance_id,
            self.child_pid,
        )?;
        crate::host_daemon_registration::release_generation(
            &self.paths,
            &self.instance_id,
            std::process::id(),
        )
    }
    async fn reap_child_after_failure(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        return crate::host_lifecycle_shutdown::reap_child_after_failure(&mut self.child, false)
            .await;
        #[cfg(windows)]
        {
            if self.child.try_wait()?.is_none() {
                self.child.start_kill()?;
                self.wait_for_exit(std::time::Duration::from_secs(5))
                    .await?;
            }
            Ok(())
        }
    }
    fn dispose_public_endpoint(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        if let Some(endpoint) = self.public_endpoint.take() {
            return endpoint.dispose();
        }
        Ok(())
    }
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
