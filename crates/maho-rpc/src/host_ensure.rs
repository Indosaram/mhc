use std::collections::HashMap;
pub struct LockedHostEnsure {
    pub paths: crate::host_daemon_paths::HostDaemonPaths,
    pub lock: crate::ownership_safe_lock::OwnershipSafeLock,
    pub registration: Option<crate::host_daemon_registration::RegisteredHost>,
    pub protocol: Option<crate::host_protocol_info::HostProtocolInfo>,
    pub decision: crate::host_decision::HostDecision,
    /// The client and policy the decision was made for, so a stopping generation's exit re-runs it.
    pub client: crate::host_decision::HostDecisionClient,
    pub policy: crate::host_decision::HostDecisionPolicy,
    /// senpi `owner: "caller"`: a reused or handed-off generation takes this caller as its lifetime owner.
    pub owner: bool,
    /// senpi `stoppingTimeoutMs`: how long a matching stopping generation is waited out, capped by the stop budget.
    pub stopping_timeout_ms: Option<u64>,
}
pub struct StartedHost {
    pub child: tokio::process::Child,
    pub protocol: crate::host_protocol_info::HostProtocolInfo,
    pub instance_id: String,
}
pub struct HostStartOptions {
    pub env: HashMap<String, String>,
    pub settings: crate::host_daemon_state::HostDaemonSettings,
    pub launch_profile_id: String,
    pub timeout: std::time::Duration,
    /// senpi `owner: "caller"`: the generation records this caller as its lifetime owner (`owner.json`); `None`
    /// records it explicitly unowned, so a later owner may claim it.
    ///
    /// ADAPTATION: senpi also hands the supervisor the caller's lifetime pipe (`--owner-fd 3`). Rust's `Command`
    /// exposes only the standard slots and this crate denies the `unsafe` a `pre_exec` `dup2` needs, so the
    /// supervisor binds the recorded owner by its OS start identity and probes it (`SupervisorOwner`).
    pub owner: Option<crate::host_lifecycle_activity::HostLifetimeOwner>,
}
pub enum EnsuredHost {
    Reused { pid: u32 },
    Started(StartedHost),
    HandedOff(crate::host_successor::RunningSuccessor),
    Refused(&'static str),
    ForeignWriter(HostHolder),
}
pub async fn ensure_prepared_host(
    mut prepared: LockedHostEnsure,
    socket: &str,
    agent_dir: &std::path::Path,
    launch: &crate::host_launch::HostLaunch,
    mut start: HostStartOptions,
    handoff: crate::host_handoff::HandoffOptions,
) -> std::io::Result<EnsuredHost> {
    use crate::host_decision::{HostDecision, RefuseReason};
    let socket = normalize_socket_path(socket);
    // senpi `stopDeadline`: one budget for every stopping generation this ensure waits out.
    let budget_ms = crate::host_ensure_stop::stop_wait_budget_ms(&std::env::vars().collect());
    let stop_deadline = tokio::time::Instant::now()
        + std::time::Duration::from_millis(
            prepared
                .stopping_timeout_ms
                .map_or(budget_ms, |timeout| timeout.min(budget_ms)),
        );
    // senpi `ensureHostLocked`: a matching stopping generation is waited out and discovery runs again under
    // the same lock; anything else stopping is refused.
    while matches!(
        prepared.decision,
        HostDecision::Refuse {
            reason: RefuseReason::HostStopping,
            ..
        }
    ) && prepared.protocol.is_some()
    {
        if !wait_out_stopping_generation(&prepared, socket, stop_deadline).await {
            return Ok(EnsuredHost::Refused("host_stopping"));
        }
        let (registration, protocol, decision) = discover_host(
            &prepared.paths,
            socket,
            &mut prepared.client,
            prepared.policy,
        )
        .await?;
        prepared.registration = registration;
        prepared.protocol = protocol;
        prepared.decision = decision;
    }
    // senpi `owner: "caller"`: a reused or handed-off generation takes this caller as its lifetime owner, inside
    // the lock; a live other owner refuses the ensure.
    if prepared.owner
        && prepared.protocol.is_some()
        && matches!(
            prepared.decision,
            HostDecision::Reuse { .. } | HostDecision::Handoff { .. }
        )
    {
        claim_ensured_owner(&prepared, socket)?;
    }
    let attached_pid = prepared
        .registration
        .as_ref()
        .filter(|owner| {
            owner
                .socket
                .as_deref()
                .is_none_or(|registered| registered == socket)
        })
        .map_or(0, |owner| owner.pid);
    // senpi `ensureHostLocked`: a registered supervisor that is gone while its host child still runs is a
    // generation nothing may replace (IS-5).
    if let Some(registered) = prepared.registration.as_ref().filter(|owner| {
        owner
            .socket
            .as_deref()
            .is_none_or(|registered| registered == socket)
    }) && !crate::host_reservations::process_is_live(registered.pid)
        && crate::host_stalled_evidence::host_child_alive(
            &crate::host_daemon_paths::generation_paths(&prepared.paths, &registered.instance_id),
            |line| eprintln!("{line}"),
        ) == Some(true)
    {
        return Ok(EnsuredHost::Refused("host_stalled"));
    }
    match prepared.decision {
        HostDecision::Reuse { .. } => return Ok(EnsuredHost::Reused { pid: attached_pid }),
        HostDecision::Refuse { reason, .. } => {
            return Ok(EnsuredHost::Refused(match reason {
                RefuseReason::Protocol => "protocol",
                RefuseReason::Capability => "capability",
                RefuseReason::HostStopping => "host_stopping",
            }));
        }
        HostDecision::Handoff { .. } => {
            // senpi `upgradeGeneration` -> `handoffHostLocked`: the upgrade runs under this ensure's lock, so no
            // second ensure can start or stop beside it. A refused handoff attaches.
            let result = crate::host_handoff::handoff_host_locked(socket, agent_dir, handoff).await;
            prepared.lock.release().map_err(std::io::Error::other)?;
            return match result? {
                Ok(successor) => Ok(EnsuredHost::HandedOff(successor)),
                Err(_) => Ok(EnsuredHost::Reused { pid: attached_pid }),
            };
        }
        HostDecision::Start { .. } => {}
        HostDecision::Fallback { .. } => return Ok(EnsuredHost::Reused { pid: attached_pid }),
    }
    if let Some(registered) = crate::host_daemon_registration::proven_owner(
        prepared.registration.as_ref(),
        normalize_socket_path(socket),
    ) && !crate::host_daemon_registration::written_by_this_process(registered.writer.as_ref())
        && refuse_if_foreign_held(&prepared.paths, registered, normalize_socket_path(socket))
            .await
            .is_err()
    {
        // senpi `HostEnsureRefusedError.holder`: the refusal names the generation in the way.
        return Ok(EnsuredHost::ForeignWriter(read_host_holder(
            &prepared.paths,
            registered,
        )));
    }
    if let Some(reason) = start_refusal(&prepared, socket).await? {
        return Ok(EnsuredHost::Refused(reason));
    }
    let owner =
        crate::host_daemon_registration::proven_owner(prepared.registration.as_ref(), socket);
    // IS-5: a proven generation nothing reaches whose loop is measurably stalled keeps serving its sessions.
    if let Some(owner) = owner
        && crate::host_ensure_liveness::refuse_if_stalled(
            &prepared.paths,
            &owner.instance_id,
            socket,
            prepared.protocol.as_ref(),
        )
        .is_err()
    {
        return Ok(EnsuredHost::Refused("host_stalled"));
    }
    let stranded = owner.is_some_and(|owner| {
        !crate::host_daemon_registration::written_by_this_process(owner.writer.as_ref())
    });
    if let Some(owner) = owner
        && !stranded
    {
        // senpi `stopManagedHost` (host-ensure-stop.ts): the intent goes on record before the first signal.
        crate::host_ensure_stop::stop_managed_host(
            &crate::host_ensure_stop::DaemonPidFile {
                pid: owner.pid,
                process_start_time: owner.process_start_time.clone(),
            },
            crate::host_ensure_stop::default_stop_timeout_ms(&std::env::vars().collect()),
            &crate::host_ensure_stop::StopTarget {
                daemon_dir: prepared.paths.dir.clone(),
                generation: crate::host_daemon_paths::generation_paths(
                    &prepared.paths,
                    &owner.instance_id,
                ),
                instance_id: owner.instance_id.clone(),
                sender: crate::host_ensure_stop::ensure_sender(),
                reason: "replace_unreachable".into(),
            },
            crate::host_reservations::read_process_start_time,
        )
        .await?;
    }
    if stranded {
        start.settings.generation = owner.map_or(0., |owner| owner.generation + 1.);
        start.env.insert(
            crate::protocol_identity::HOST_GENERATION_ENV.into(),
            start.settings.generation.to_string(),
        );
    } else {
        // Something no registration names still ACCEPTS at the public path and did not answer the probe: a
        // start could only lose the bind, and the holder is not ours to remove (I1).
        if crate::host_ensure_liveness::unregistered_endpoint_held(socket).await {
            return Ok(EnsuredHost::Refused("endpoint_occupied"));
        }
        if prepared.registration.as_ref().is_some_and(|owner| {
            owner
                .socket
                .as_deref()
                .is_none_or(|registered| registered == socket)
        }) {
            crate::host_daemon_registration::clear_host_registration(&prepared.paths)?;
        }
    }
    let started = start_host(&prepared, socket, launch, start).await?;
    prepared.lock.release().map_err(std::io::Error::other)?;
    Ok(EnsuredHost::Started(started))
}
#[cfg(unix)]
pub async fn stop_managed_host(
    owner: &crate::host_daemon_registration::RegisteredHost,
    timeout: std::time::Duration,
) -> std::io::Result<()> {
    for signal in [rustix::process::Signal::TERM, rustix::process::Signal::KILL] {
        let owned = owner.process_start_time.as_ref().is_some_and(|recorded| {
            crate::host_reservations::read_process_start_time(owner.pid).as_ref() == Some(recorded)
        });
        if !owned {
            return if crate::host_reservations::process_is_live(owner.pid) {
                Err(std::io::Error::other("host process identity is unknown"))
            } else {
                Ok(())
            };
        }
        crate::host_stop::signal_generation(owner.pid, signal).map_err(std::io::Error::from)?;
        let deadline = tokio::time::Instant::now()
            + if signal == rustix::process::Signal::TERM {
                timeout
            } else {
                std::time::Duration::from_secs(2)
            };
        loop {
            if !crate::host_reservations::process_is_live(owner.pid)
                || owner.process_start_time.as_ref().is_some_and(|recorded| {
                    crate::host_reservations::read_process_start_time(owner.pid)
                        .as_ref()
                        .is_some_and(|current| current != recorded)
                })
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
    Err(std::io::Error::other("host remained alive after SIGKILL"))
}
/// Windows: Node's SIGTERM and SIGKILL both map to TerminateProcess, so one terminate is followed by the
/// same bounded wait for the recorded generation to be gone.
#[cfg(windows)]
pub async fn stop_managed_host(
    owner: &crate::host_daemon_registration::RegisteredHost,
    timeout: std::time::Duration,
) -> std::io::Result<()> {
    let owned = owner.process_start_time.as_ref().is_some_and(|recorded| {
        crate::host_reservations::read_process_start_time(owner.pid).as_ref() == Some(recorded)
    });
    if !owned {
        return if crate::host_reservations::process_is_live(owner.pid) {
            Err(std::io::Error::other("host process identity is unknown"))
        } else {
            Ok(())
        };
    }
    crate::host_stop::terminate_generation(owner.pid)?;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if !crate::host_reservations::process_is_live(owner.pid)
            || owner.process_start_time.as_ref().is_some_and(|recorded| {
                crate::host_reservations::read_process_start_time(owner.pid)
                    .as_ref()
                    .is_some_and(|current| current != recorded)
            })
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(std::io::Error::other(
                "host remained alive after TerminateProcess",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
#[cfg(not(any(unix, windows)))]
pub async fn stop_managed_host(
    _: &crate::host_daemon_registration::RegisteredHost,
    _: std::time::Duration,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stopping managed hosts is unsupported on this platform",
    ))
}
pub fn start_host(
    prepared: &LockedHostEnsure,
    socket: &str,
    launch: &crate::host_launch::HostLaunch,
    options: HostStartOptions,
) -> impl std::future::Future<Output = std::io::Result<StartedHost>> + Send + use<> {
    let paths = prepared.paths.clone();
    let socket = normalize_socket_path(socket).to_owned();
    let launch = launch.clone();
    async move {
    let socket = socket.as_str();
    let HostStartOptions {
        env,
        settings,
        launch_profile_id,
        timeout,
        owner,
    } = options;
    let socket = normalize_socket_path(socket);
    crate::host_daemon_state::write_host_settings(&paths, &settings)
        .map_err(|error| error.source)?;
    crate::host_lifecycle_activity::write_host_owner(
        &crate::host_daemon_paths::generation_paths(&paths, &settings.instance_id).dir,
        owner.as_ref(),
    )?;
    let mut stderr_options = std::fs::OpenOptions::new();
    stderr_options
        .create(true)
        .write(true)
        .truncate(settings.generation == 0.)
        .append(settings.generation != 0.);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        stderr_options.mode(0o600);
    }
    let stderr = stderr_options.open(&paths.stderr_log)?;
    // senpi `startHost`: the presenter channel enrolls this initial launch, and enforcement is the deployment opt-in.
    let bootstrap = crate::mcp_presenter_bootstrap::take_presenter_bootstrap();
    let args: Vec<String> = launch
        .args
        .iter()
        .cloned()
        .chain(crate::mcp_presenter_bootstrap::presenter_launch_args(
            bootstrap.is_some(),
            crate::mcp_presenter_bootstrap::presenter_enforcement(),
        ))
        .collect();
    let configure = |command: &mut tokio::process::Command| {
        command
            .env_clear()
            .envs(env)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(stderr);
        #[cfg(unix)]
        command.process_group(0);
    };
    #[cfg(unix)]
    let (mut child, presenter_enrollment) =
        crate::mcp_presenter_bootstrap::spawn_with_presenter_bootstrap(
            launch.command.as_os_str(),
            &args,
            configure,
            bootstrap,
            crate::mcp_presenter_bootstrap::PresenterLaunch {
                kind: "initial",
                predecessor_host_instance: Some(None),
            },
        )?;
    #[cfg(not(unix))]
    let (mut child, presenter_enrollment) = {
        drop(bootstrap);
        let mut command = tokio::process::Command::new(&launch.command);
        command.args(&args);
        configure(&mut command);
        (command.spawn()?, None::<tokio::task::JoinHandle<()>>)
    };
    let pid = child
        .id()
        .ok_or_else(|| std::io::Error::other("RPC socket host has no process identity"))?;
    let result = async {
        let registration = crate::host_daemon_registration::HostRegistration {
            pid,
            process_start_time: crate::host_reservations::read_process_start_time(pid),
            socket: socket.into(),
            instance_id: settings.instance_id.clone(),
            generation: settings.generation,
            launch_profile_id,
            build: Some(crate::host_daemon_registration::RegisteredBuild::this_engine()),
        };
        crate::host_daemon_registration::write_host_registration(&paths, &registration)
            .map_err(|error| error.source)?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                return Err(std::io::Error::other(format!(
                    "RPC socket host exited before ready ({status})"
                )));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "RPC socket host did not become ready",
                ));
            }
            let probe_ms = remaining.as_millis().min(1000) as u64;
            if let Some(protocol) = crate::host_probe::probe_protocol_info(socket, probe_ms).await
                && protocol.instance_id.as_deref() == Some(settings.instance_id.as_str())
            {
                return Ok(protocol);
            }
            tokio::time::sleep(
                std::time::Duration::from_millis(50)
                    .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
        }
    }
    .await;
    match result {
        Ok(protocol) => {
            // senpi `await presenterEnrollment` once the host is ready.
            if let Some(enrollment) = presenter_enrollment {
                let _ = enrollment.await;
            }
            Ok(StartedHost {
                child,
                protocol,
                instance_id: settings.instance_id,
            })
        }
        Err(error) => {
            if let Some(enrollment) = presenter_enrollment {
                enrollment.abort();
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            crate::host_daemon_registration::release_generation(
                &paths,
                &settings.instance_id,
                pid,
            )?;
            Err(error)
        }
    }
    }
}
pub fn start_refusal(
    prepared: &LockedHostEnsure,
    socket: &str,
) -> impl std::future::Future<Output = std::io::Result<Option<&'static str>>> + Send + use<> {
    let paths = prepared.paths.clone();
    let registration = prepared.registration.clone();
    let socket = normalize_socket_path(socket).to_owned();
    async move {
    let socket = socket.as_str();
    if let Some(registered) =
        crate::host_daemon_registration::proven_owner(registration.as_ref(), socket)
    {
        if crate::host_daemon_registration::written_by_this_process(registered.writer.as_ref()) {
            if crate::host_probe::probe_socket_reachable(socket, 2000).await {
                return Ok(Some("host_busy"));
            }
        } else if refuse_if_foreign_held(&paths, registered, socket)
            .await
            .is_err()
        {
            return Ok(Some("foreign_writer"));
        }
    }
    let legacy = crate::host_daemon_state::read_file_or_undefined(&paths.legacy_pid_file)?;
    if let Some(legacy) = crate::host_daemon_state::parse_json(legacy.as_deref())
        && let Some(pid) = legacy
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
    {
        let recorded = legacy
            .get("processStartTime")
            .and_then(serde_json::Value::as_str);
        if crate::host_reservations::process_is_live(pid)
            && recorded.is_none_or(|recorded| {
                crate::host_reservations::read_process_start_time(pid).as_deref() == Some(recorded)
            })
        {
            return Ok(Some("legacy_host"));
        }
    }
    Ok(None)
    }
}
pub use crate::host_holder::HostHolder;
/// The `foreign_writer` refusal: the endpoint and the host it leaves in the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignWriterRefusal {
    pub socket: String,
    pub holder: HostHolder,
}
/// The registration's own account of the generation: its record also stores the build and launch profile.
pub fn read_host_holder(
    paths: &crate::host_daemon_paths::HostDaemonPaths,
    registered: &crate::host_daemon_registration::RegisteredHost,
) -> HostHolder {
    let text = crate::host_daemon_state::read_file_or_undefined(
        &crate::host_daemon_paths::generation_paths(paths, &registered.instance_id).pid_file,
    )
    .ok()
    .flatten();
    let record = crate::host_daemon_state::parse_json(text.as_deref()).unwrap_or_default();
    let text_field = |key: &str| {
        record
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    HostHolder {
        pid: registered.pid,
        instance_id: registered.instance_id.clone(),
        generation: registered.generation as u64,
        process_start_time: registered.process_start_time.clone(),
        engine_version: text_field("engineVersion"),
        launch_profile_id: text_field("launchProfileId"),
        writer_pid: registered
            .writer
            .as_ref()
            .and_then(|writer| writer["pid"].as_u64())
            .and_then(|pid| u32::try_from(pid).ok()),
    }
}
/// I1 for a record another process wrote: while something still accepts at the public path, the generation it
/// names may own the endpoint, so the ensure refuses `foreign_writer` - and the refusal names that generation, so
/// a client can say WHO is in the way. An endpoint nothing accepts at is left to the caller's stranded start.
pub async fn refuse_if_foreign_held(
    paths: &crate::host_daemon_paths::HostDaemonPaths,
    registered: &crate::host_daemon_registration::RegisteredHost,
    socket: &str,
) -> Result<(), ForeignWriterRefusal> {
    if !public_endpoint_accepts(socket).await {
        return Ok(());
    }
    Err(ForeignWriterRefusal {
        socket: socket.into(),
        holder: read_host_holder(paths, registered),
    })
}
pub async fn prepare_ensure_host(
    socket: &str,
    agent_dir: &std::path::Path,
    mut client: crate::host_decision::HostDecisionClient,
    policy: crate::host_decision::HostDecisionPolicy,
) -> Result<LockedHostEnsure, crate::ownership_safe_lock::LockError> {
    let socket = normalize_socket_path(socket);
    let paths = crate::host_daemon_paths::create_host_daemon_paths(socket, agent_dir);
    crate::host_daemon_paths::create_daemon_directories(&paths).map_err(|error| error.source)?;
    let lock = crate::ownership_safe_lock::acquire_ownership_safe_lock(
        &paths.lock_file,
        Default::default(),
    )
    .await?;
    let policy = if policy == crate::host_decision::HostDecisionPolicy::Upgrade {
        policy
    } else {
        crate::host_decision::HostDecisionPolicy::Never
    };
    let (registration, protocol, decision) =
        discover_host(&paths, socket, &mut client, policy).await?;
    Ok(LockedHostEnsure {
        paths,
        lock,
        registration,
        protocol,
        decision,
        client,
        policy,
        owner: false,
        stopping_timeout_ms: None,
    })
}
/// The discovery an ensure runs under its lock: the registration, the running host's answer, and the decision.
async fn discover_host(
    paths: &crate::host_daemon_paths::HostDaemonPaths,
    socket: &str,
    client: &mut crate::host_decision::HostDecisionClient,
    policy: crate::host_decision::HostDecisionPolicy,
) -> std::io::Result<(
    Option<crate::host_daemon_registration::RegisteredHost>,
    Option<crate::host_protocol_info::HostProtocolInfo>,
    crate::host_decision::HostDecision,
)> {
    let registration = crate::host_daemon_registration::read_host_registration(paths)?;
    let registered_here = registration.as_ref().filter(|registered| {
        registered
            .socket
            .as_deref()
            .is_none_or(|registered| registered == socket)
    });
    client.started_by_us = registered_here.is_some_and(|registered| {
        crate::host_daemon_registration::written_by_this_process(registered.writer.as_ref())
    });
    let protocol = crate::host_probe::probe_protocol_info(socket, 2000).await;
    let mut decision = crate::host_decision::decide_host_action(client, protocol.as_ref(), policy);
    if matches!(
        decision,
        crate::host_decision::HostDecision::Fallback { .. }
    ) {
        decision = crate::host_decision::HostDecision::Reuse {
            reason: crate::host_decision::ReuseReason::Compatible,
            upgradeable: false,
            warning: None,
        };
    }
    Ok((registration, protocol, decision))
}
/// senpi `ensureHostLocked`'s stopping branch: a typed stop is not permission to signal or overwrite a foreign
/// generation, so only the registered generation the stopping host names is waited out - both its supervisor
/// and its host child gone - and discovery then runs again under the same lock. `false` refuses `host_stopping`.
async fn wait_out_stopping_generation(
    prepared: &LockedHostEnsure,
    socket: &str,
    deadline: tokio::time::Instant,
) -> bool {
    // senpi `registeredHere`: the record about this endpoint, live or not - the wait is for it to be gone.
    let Some(registered) = prepared.registration.as_ref().filter(|registered| {
        registered
            .socket
            .as_deref()
            .is_none_or(|recorded| recorded == socket)
    }) else {
        return false;
    };
    if prepared
        .protocol
        .as_ref()
        .and_then(|protocol| protocol.instance_id.as_deref())
        != Some(registered.instance_id.as_str())
    {
        return false;
    }
    let pid_file = crate::host_ensure_stop::DaemonPidFile {
        pid: registered.pid,
        process_start_time: registered.process_start_time.clone(),
    };
    let generation =
        crate::host_daemon_paths::generation_paths(&prepared.paths, &registered.instance_id);
    loop {
        let gone = crate::host_ensure_stop::resolve_pid_file_ownership(
            &pid_file,
            crate::host_reservations::read_process_start_time,
        ) == crate::host_ensure_stop::PidFileOwnership::Gone;
        if gone && crate::host_stalled_evidence::host_child_alive(&generation, |_| {}) != Some(true)
        {
            return true;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        tokio::time::sleep(remaining.min(std::time::Duration::from_millis(50))).await;
    }
}
/// senpi `claimHostOwner` for a reused or handed-off generation: only the registered generation that answered.
fn claim_ensured_owner(prepared: &LockedHostEnsure, socket: &str) -> std::io::Result<()> {
    let registered =
        crate::host_daemon_registration::proven_owner(prepared.registration.as_ref(), socket)
            .filter(|registered| {
                prepared
                    .protocol
                    .as_ref()
                    .and_then(|protocol| protocol.instance_id.as_deref())
                    == Some(registered.instance_id.as_str())
            })
            .ok_or_else(|| {
                std::io::Error::other(
                    "RPC host has no matching registered owner-lifetime generation",
                )
            })?;
    crate::host_lifecycle_activity::claim_host_owner(
        &crate::host_daemon_paths::generation_paths(&prepared.paths, &registered.instance_id).dir,
        crate::host_lifecycle_activity::caller_host_owner()?,
    )
}
pub fn normalize_socket_path(value: &str) -> &str {
    value.strip_prefix("unix://").unwrap_or(value)
}
pub async fn public_endpoint_accepts(socket: &str) -> bool {
    if socket.starts_with('\0') {
        return true;
    }
    match crate::socket_ownership::stat_socket_identity(std::path::Path::new(socket)) {
        Ok(None) => false,
        Err(_) => true,
        Ok(Some(_)) => crate::host_probe::probe_socket_reachable(socket, 2000).await,
    }
}
pub fn host_child_argv(args: &[String]) -> Vec<String> {
    ["--mode", "rpc", "--multi-session"]
        .into_iter()
        .map(str::to_owned)
        .chain(args.iter().cloned())
        .collect()
}
/// Whether `start_host` failed because the spawned host never came up (senpi `ensureHost` refuses
/// rather than reporting an opaque IO error for a host it could not bring online).
fn is_host_start_failure(error: &std::io::Error) -> bool {
    let message = error.to_string();
    message.starts_with("RPC socket host exited before ready")
        || message.starts_with("RPC socket host did not become ready")
}

pub use crate::host_ensure_types::{EnsureHostOptions, EnsuredHostInfo, HostUpgradePolicy};
#[derive(Debug, thiserror::Error)]
pub enum EnsureHostError {
    #[error("host ensure refused: {0}")]
    Refused(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
/** The supervisor argv an ensure hands its spawned host, matching `parse_supervisor_args`. */
pub fn ensure_supervisor_args(socket: &str, agent_dir: &str, host_args: &[String]) -> Vec<String> {
    [
        "--socket".into(),
        socket.into(),
        "--agent-dir".into(),
        agent_dir.into(),
    ]
    .into_iter()
    .chain(host_args.iter().cloned())
    .collect()
}
/**
 * Ensure a host serves `options.socket`, starting one when nothing compatible is there.
 *
 * Composes the locked ownership preflight with the spawn/handoff orchestration so a client
 * gets one call that either returns a live endpoint or refuses without touching somebody
 * else's host (senpi `ensureHost`).
 */
pub async fn ensure_host(options: EnsureHostOptions) -> Result<EnsuredHostInfo, EnsureHostError> {
    let root = crate::host_probe::rpc_key_root(
        options.key_root.as_deref(),
        options.agent_dir.as_deref(),
    );
    let socket = normalize_socket_path(&options.socket).to_owned();
    let agent_dir = options
        .agent_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from(maho_core::config::get_agent_dir()));
    let ensured = crate::host_probe::with_rpc_key_root(root, ensure_host_in_key_root(options)).await;
    // Off the awaited path and outside the lock: the caller gets its host first, then dead endpoint records under
    // this agent dir are reaped in the background on `host gc`'s own evidence. A failed ensure schedules nothing.
    if ensured.is_ok() {
        crate::host_gc_pass::schedule_opportunistic_host_gc(
            crate::host_gc_pass::OpportunisticGcOptions::new(agent_dir, socket),
        );
    }
    ensured
}
async fn ensure_host_in_key_root(
    options: EnsureHostOptions,
) -> Result<EnsuredHostInfo, EnsureHostError> {
    let socket = normalize_socket_path(&options.socket).to_owned();
    let agent_dir = options
        .agent_dir
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from(maho_core::config::get_agent_dir()));
    let daemon_dir = crate::host_daemon_paths::create_host_daemon_paths(&socket, &agent_dir).dir;
    let instance_id = crate::protocol_identity::resolve_instance_id(
        std::env::var(crate::protocol_identity::HOST_INSTANCE_ID_ENV)
            .ok()
            .as_deref(),
    );
    let generation = 0u64;
    let build = maho_core::engine_build_identity::engine_build_identity().clone();
    let launch_profile = crate::protocol_identity::launch_profile_from_core(
        crate::host_protocol_info::RpcLaunchProfileCore {
            extensions: options.extensions.clone(),
            multi_session: true,
            session_runtime: crate::host_protocol_info::SessionRuntimeKind::InProcess,
        },
    )
    .ok();
    let client = crate::host_decision::HostDecisionClient {
        protocol_version: crate::host_decision::HOST_PROTOCOL_VERSION,
        required_capabilities: crate::host_decision::REQUIRED_HOST_CAPABILITIES
            .iter()
            .map(|value| (*value).into())
            .collect(),
        identity: build,
        launch_profile: launch_profile.clone(),
        started_by_us: false,
        platform: if cfg!(windows) {
            "win32"
        } else {
            std::env::consts::OS
        }
        .into(),
    };
    let decision_policy = match options.upgrade {
        HostUpgradePolicy::Never => crate::host_decision::HostDecisionPolicy::Never,
        HostUpgradePolicy::IfEngineDiffers => crate::host_decision::HostDecisionPolicy::Upgrade,
    };
    let mut prepared = prepare_ensure_host(&socket, &agent_dir, client, decision_policy)
        .await
        .map_err(|error| EnsureHostError::Io(std::io::Error::other(error.to_string())))?;
    prepared.owner = options.owner;
    prepared.stopping_timeout_ms = options.stopping_timeout_ms;
    let supervisor_args =
        ensure_supervisor_args(&socket, &agent_dir.to_string_lossy(), &options.host_args);
    let launch = crate::host_launch::default_host_launch(&supervisor_args)?;
    let lifecycle = options.policy.clone().unwrap_or_default();
    let host_policy = crate::host_lifecycle::HostLifecyclePolicy {
        cold_start: lifecycle
            .cold_start
            .clone()
            .unwrap_or_else(|| "transient".into()),
        idle_exit_ms: lifecycle
            .idle_exit_ms
            .unwrap_or(crate::host_lifecycle::DEFAULT_HOST_IDLE_EXIT_MS),
    };
    let settings = crate::host_daemon_state::HostDaemonSettings {
        socket: socket.clone(),
        capabilities: crate::host_launch::PINNED_HOST_CLIENT_CAPABILITIES
            .iter()
            .map(|value| (*value).into())
            .collect(),
        cold_start: host_policy.cold_start.clone(),
        idle_exit_ms: host_policy.idle_exit_ms,
        generation: generation as f64,
        instance_id: instance_id.clone(),
    };
    let env = host_env(
        std::env::vars().collect(),
        &options.env,
        &agent_dir.to_string_lossy(),
        &daemon_dir.to_string_lossy(),
        &instance_id,
        generation,
    );
    let launch_profile_id = launch_profile.map_or_else(String::new, |profile| profile.profile_id);
    let start = HostStartOptions {
        env: env.clone(),
        settings,
        launch_profile_id: launch_profile_id.clone(),
        timeout: std::time::Duration::from_millis(options.readiness_ms),
        owner: if options.owner {
            Some(crate::host_lifecycle_activity::caller_host_owner()?)
        } else {
            None
        },
    };
    let handoff = crate::host_handoff::HandoffOptions {
        host_args: supervisor_args,
        policy: Some(host_policy),
        env: options.env.clone(),
        expected_runtime_build_id: None,
        launch_profile_id,
        readiness_ms: options.readiness_ms,
        launch: None,
    };
    let outcome =
        match ensure_prepared_host(prepared, &socket, &agent_dir, &launch, start, handoff).await {
            Ok(outcome) => outcome,
            // `start_host` reports a host that never became ready with these messages; that is the
            // ensure's refusal (it could not produce the endpoint it was asked for), not a caller-facing
            // IO error. Any other failure - a missing launch binary, a permission or address conflict -
            // stays an IO error so a real resource problem is never masked as a refusal.
            Err(error) if is_host_start_failure(&error) => {
                return Err(EnsureHostError::Refused("host_unavailable"));
            }
            Err(error) => return Err(EnsureHostError::Io(error)),
        };
    match outcome {
        EnsuredHost::Refused(reason) => Err(EnsureHostError::Refused(reason)),
        EnsuredHost::ForeignWriter(_) => Err(EnsureHostError::Refused("foreign_writer")),
        EnsuredHost::Reused { pid } => Ok(EnsuredHostInfo {
            pid,
            socket,
            reused: true,
        }),
        EnsuredHost::Started(host) => Ok(EnsuredHostInfo {
            pid: host.child.id().unwrap_or_default(),
            socket,
            reused: false,
        }),
        EnsuredHost::HandedOff(host) => Ok(EnsuredHostInfo {
            pid: host.child.id().unwrap_or_default(),
            socket,
            reused: false,
        }),
    }
}
pub fn host_env(
    mut env: HashMap<String, String>,
    overrides: &HashMap<String, Option<String>>,
    agent_dir: &str,
    daemon_dir: &str,
    instance_id: &str,
    generation: u64,
) -> HashMap<String, String> {
    for (key, value) in overrides {
        if let Some(value) = value {
            env.insert(key.clone(), value.clone());
        } else {
            env.remove(key);
        }
    }
    env.insert(maho_core::config::env_agent_dir_var(), agent_dir.into());
    env.insert(
        crate::custom_capability::RPC_CLIENT_CAPABILITIES_ENV.into(),
        crate::host_launch::PINNED_HOST_CLIENT_CAPABILITIES.join(","),
    );
    env.insert(
        crate::protocol_identity::HOST_INSTANCE_ID_ENV.into(),
        instance_id.into(),
    );
    env.insert(
        crate::protocol_identity::HOST_GENERATION_ENV.into(),
        generation.to_string(),
    );
    env.insert(
        crate::host_daemon_paths::HOST_DAEMON_DIR_ENV.into(),
        daemon_dir.into(),
    );
    env
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ensure_supervisor_argv_names_endpoint_and_agent_dir() {
        assert_eq!(
            ensure_supervisor_args("/tmp/s", "/agent", &["--permission".into(), "read".into()]),
            vec![
                "--socket",
                "/tmp/s",
                "--agent-dir",
                "/agent",
                "--permission",
                "read"
            ]
        );
        assert_eq!(
            ensure_supervisor_args("unix:///tmp/s", "/agent", &[]),
            vec!["--socket", "unix:///tmp/s", "--agent-dir", "/agent"]
        );
    }
    #[test]
    fn ensure_options_default_to_never_upgrade_and_bounded_readiness() {
        let options = EnsureHostOptions::default();
        assert_eq!(options.upgrade, HostUpgradePolicy::Never);
        assert_eq!(options.readiness_ms, 10_000);
    }
    async fn stopping_ensure(
        temp: &std::path::Path,
        socket: &str,
        registered_instance: &str,
        stopping_instance: &str,
    ) -> LockedHostEnsure {
        let paths = crate::host_daemon_paths::create_host_daemon_paths(socket, temp);
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        let lock = crate::ownership_safe_lock::acquire_ownership_safe_lock(
            &temp.join(format!("{registered_instance}-{stopping_instance}.lock")),
            Default::default(),
        )
        .await
        .unwrap();
        LockedHostEnsure {
            paths,
            lock,
            registration: Some(crate::host_daemon_registration::RegisteredHost {
                // No process holds pid u32::MAX, so the registered supervisor reads as gone.
                pid: u32::MAX,
                process_start_time: Some("start".into()),
                socket: Some(socket.into()),
                instance_id: registered_instance.into(),
                generation: 0.,
                writer: None,
            }),
            protocol: crate::host_protocol_info::parse_host_protocol_info(&serde_json::json!({
                "protocolVersion": 1, "serverVersion": "1", "capabilities": [],
                "instanceId": stopping_instance, "lifecycle": "stopping"
            })),
            decision: crate::host_decision::HostDecision::Refuse {
                reason: crate::host_decision::RefuseReason::HostStopping,
                upgradeable: false,
            },
            client: crate::host_decision::HostDecisionClient {
                protocol_version: 1,
                required_capabilities: vec![],
                identity: maho_core::engine_build_identity::engine_build_identity().clone(),
                launch_profile: None,
                started_by_us: false,
                platform: "linux".into(),
            },
            policy: crate::host_decision::HostDecisionPolicy::Never,
            owner: false,
            stopping_timeout_ms: Some(0),
        }
    }
    /// senpi `ensureHostLocked`: only the registered generation the stopping host names is waited out, and one
    /// whose supervisor and host child are both gone ends the wait at once; a foreign stopping host refuses.
    #[tokio::test(start_paused = true)]
    async fn only_the_registered_stopping_generation_is_waited_out() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("h.sock");
        let socket = socket.to_str().unwrap();
        let deadline = tokio::time::Instant::now();
        let ours = stopping_ensure(temp.path(), socket, "gen-a", "gen-a").await;
        assert!(wait_out_stopping_generation(&ours, socket, deadline).await);
        let foreign = stopping_ensure(temp.path(), socket, "gen-a", "gen-b").await;
        assert!(!wait_out_stopping_generation(&foreign, socket, deadline).await);
        let mut elsewhere = stopping_ensure(temp.path(), socket, "gen-c", "gen-c").await;
        if let Some(registered) = elsewhere.registration.as_mut() {
            registered.socket = Some("/other.sock".into());
        }
        assert!(!wait_out_stopping_generation(&elsewhere, socket, deadline).await);
    }
    #[tokio::test]
    async fn ensure_refuses_a_foreign_writer_without_starting() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("absent.sock");
        let outcome = ensure_host(EnsureHostOptions {
            socket: socket.to_string_lossy().into_owned(),
            agent_dir: Some(temp.path().to_path_buf()),
            readiness_ms: 200,
            ..Default::default()
        })
        .await;
        assert!(outcome.is_ok() || matches!(outcome, Err(EnsureHostError::Refused(_))));
    }
    // senpi host-ensure.ts: the gc pass is scheduled after an ensure returned its host, never after a failure.
    // A current-thread runtime cannot run a scheduled pass before the check, so a scheduled one is still pending.
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_ensure_schedules_no_gc_pass() {
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path().join("not-a-dir");
        std::fs::write(&agent_dir, "").unwrap();
        let outcome = ensure_host(EnsureHostOptions {
            socket: temp.path().join("s.sock").to_string_lossy().into_owned(),
            agent_dir: Some(agent_dir.clone()),
            readiness_ms: 200,
            ..Default::default()
        })
        .await;
        assert!(outcome.is_err());
        assert!(!crate::host_gc_pass::opportunistic_host_gc_pending(
            &agent_dir
        ));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn foreign_held_refusal_names_the_registered_holder_only_while_the_endpoint_accepts() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("s.sock");
        let socket = socket.to_string_lossy().into_owned();
        let paths = crate::host_daemon_paths::create_host_daemon_paths(&socket, temp.path());
        crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();
        let generation = crate::host_daemon_paths::generation_paths(&paths, "inst");
        crate::host_daemon_paths::create_generation_directory(&generation).unwrap();
        std::fs::write(
            &generation.pid_file,
            r#"{"pid":9,"engineVersion":"1.2.3","launchProfileId":"p"}"#,
        )
        .unwrap();
        let registered = crate::host_daemon_registration::RegisteredHost {
            pid: 9,
            process_start_time: Some("5".into()),
            socket: None,
            instance_id: "inst".into(),
            generation: 2.,
            writer: Some(serde_json::json!({"pid":4,"startTime":null})),
        };
        assert_eq!(
            refuse_if_foreign_held(&paths, &registered, &socket).await,
            Ok(())
        );
        let _listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let refusal = refuse_if_foreign_held(&paths, &registered, &socket)
            .await
            .unwrap_err();
        assert_eq!(
            refusal.holder,
            HostHolder {
                pid: 9,
                instance_id: "inst".into(),
                generation: 2,
                process_start_time: Some("5".into()),
                engine_version: Some("1.2.3".into()),
                launch_profile_id: Some("p".into()),
                writer_pid: Some(4)
            }
        );
        std::fs::remove_file(&generation.pid_file).unwrap();
        let bare = read_host_holder(&paths, &registered);
        assert_eq!((bare.engine_version, bare.launch_profile_id), (None, None));
    }
    #[test]
    fn fixed_host_wiring_wins_over_nullable_overrides() {
        let key = crate::protocol_identity::HOST_INSTANCE_ID_ENV;
        let env = host_env(
            HashMap::from([("REMOVE".into(), "x".into())]),
            &HashMap::from([(key.into(), None), ("REMOVE".into(), None)]),
            "/agent",
            "/daemon",
            "fresh",
            0,
        );
        assert_eq!(env[key], "fresh");
        assert_eq!(env[&maho_core::config::env_agent_dir_var()], "/agent");
        assert!(!env.contains_key("REMOVE"));
    }
    #[test]
    fn logical_socket_scheme_and_child_profile_args_match() {
        assert_eq!(normalize_socket_path("unix:///tmp/s"), "/tmp/s");
        assert_eq!(normalize_socket_path("/tmp/s"), "/tmp/s");
        assert_eq!(
            host_child_argv(&["--permission".into(), "read".into()]),
            vec!["--mode", "rpc", "--multi-session", "--permission", "read"]
        );
    }
}
