#[cfg(unix)]
pub fn signal_generation(
    pid: u32,
    signal: rustix::process::Signal,
) -> Result<bool, rustix::io::Errno> {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return Ok(false);
    };
    match rustix::process::kill_process(pid, signal) {
        Ok(()) => Ok(true),
        Err(error) if error == rustix::io::Errno::SRCH => Ok(false),
        Err(error) => Err(error),
    }
}
/// Windows `process.kill(pid, "SIGTERM")`: Node maps it to TerminateProcess of that pid alone (no tree).
/// `Ok(false)` when the process is already gone (the ESRCH analogue).
#[cfg(windows)]
pub fn terminate_generation(pid: u32) -> std::io::Result<bool> {
    if pid == 0 {
        return Ok(false);
    }
    let status = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(true)
    } else if crate::host_reservations::process_is_live(pid) {
        Err(std::io::Error::other(format!(
            "taskkill failed for pid {pid}"
        )))
    } else {
        Ok(false)
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum StopDecision {
    Drain,
    Stop,
    Refuse(&'static str),
}
#[derive(Debug, PartialEq, Eq)]
pub enum StopHostResult {
    Drained { pid: u32 },
    Stopped { pid: u32 },
    Refuse { reason: &'static str },
}
/// What `stop_host_with` did: the drain answer in full (instance id, `exited`), a hard stop, or a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopHostOutcome {
    Drain(crate::host_stop_drain::DrainHostResult),
    Stopped { pid: u32 },
    Refuse { reason: &'static str },
}
/// Senpi `StopHostOptions`.
#[derive(Debug, Clone, Default)]
pub struct StopHostOptions {
    pub socket: String,
    pub agent_dir: Option<std::path::PathBuf>,
    /// Ask the host to finish its work and exit instead of terminating it.
    pub drain: bool,
    /// With `drain`: the generation to ask, by instance id, instead of the one the pointer names.
    pub instance_id: Option<String>,
    /// With `drain`: wait up to this long for the generation to exit, and report `exited`.
    pub wait_ms: Option<u64>,
    /// Terminate even when the host still reports open sessions.
    pub force: bool,
    pub timeout_ms: Option<u64>,
    /// Refuse unless the host answers both reads: a caller that judged it idle must not stop it unseen.
    pub require_answer: bool,
    /// Refuse rather than signal a generation other than the one the operator confirmed.
    pub expected_generation: Option<crate::host_stop_generation::HostStopGeneration>,
}
const BOUND_STOP_LOCK_WAIT_MS: u64 = 30_000;
/// Legacy entry: the pre-`StopHostOptions` shape, kept for callers that pass positional flags.
pub async fn stop_host(
    socket: &str,
    agent_dir: &std::path::Path,
    drain: bool,
    force: bool,
    timeout_ms: u64,
) -> std::io::Result<StopHostResult> {
    let outcome = stop_host_with(StopHostOptions {
        socket: socket.into(),
        agent_dir: Some(agent_dir.into()),
        drain,
        force,
        timeout_ms: Some(timeout_ms),
        ..Default::default()
    })
    .await?;
    Ok(match outcome {
        StopHostOutcome::Drain(crate::host_stop_drain::DrainHostResult::Drained {
            pid, ..
        }) => StopHostResult::Drained { pid },
        StopHostOutcome::Drain(crate::host_stop_drain::DrainHostResult::Refuse { reason })
        | StopHostOutcome::Refuse { reason } => StopHostResult::Refuse { reason },
        StopHostOutcome::Stopped { pid } => StopHostResult::Stopped { pid },
    })
}
/// Ends a running generation. `drain` is always permitted - it ends no work - while a hard stop requires either an
/// empty host or `force`, so an operator never silently kills another client's sessions.
pub async fn stop_host_with(options: StopHostOptions) -> std::io::Result<StopHostOutcome> {
    // A binding names the generation a HARD stop may signal; a drain targets its own instance and must not hold the lock.
    if options.expected_generation.is_some() && options.drain {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "stopHost: expectedGeneration binds a hard stop and cannot be combined with drain",
        ));
    }
    if options.expected_generation.is_none() {
        return stop_host_locked(options).await;
    }
    // Conditional operator stops take the endpoint's ensure lock so ensure/handoff cannot publish a different
    // generation between the check and the signal.
    let agent_dir = options
        .agent_dir
        .clone()
        .unwrap_or_else(|| maho_core::config::get_agent_dir().into());
    let paths = crate::host_daemon_paths::create_host_daemon_paths(&options.socket, &agent_dir);
    crate::host_daemon_paths::create_daemon_directories(&paths).map_err(|error| error.source)?;
    let retries = crate::ownership_safe_lock::LockRetries {
        retries: u32::try_from(BOUND_STOP_LOCK_WAIT_MS / 100).unwrap_or(300),
        ..Default::default()
    };
    let mut lock =
        crate::ownership_safe_lock::acquire_ownership_safe_lock(&paths.lock_file, retries)
            .await
            .map_err(std::io::Error::other)?;
    let result = stop_host_locked(options).await;
    lock.release().map_err(std::io::Error::other)?;
    result
}
fn generation_refusal(
    matched: crate::host_stop_generation::StopGenerationMatch,
) -> Option<StopHostOutcome> {
    use crate::host_stop_generation::StopGenerationMatch;
    match matched {
        StopGenerationMatch::Stale => Some(StopHostOutcome::Refuse {
            reason: "stale_generation",
        }),
        StopGenerationMatch::Unprovable => Some(StopHostOutcome::Refuse {
            reason: "unknown_owner",
        }),
        _ => None,
    }
}
/// Refuses a binding the endpoint no longer matches, before any probe or census: a caller learns its confirmation
/// is stale rather than that the host it no longer names is busy (senpi `staleStopGeneration`).
pub fn stale_stop_generation(
    socket: &str,
    agent_dir: Option<&std::path::Path>,
    expected: &crate::host_stop_generation::HostStopGeneration,
) -> std::io::Result<Option<StopHostOutcome>> {
    let agent_dir = agent_dir.map_or_else(
        || std::path::PathBuf::from(maho_core::config::get_agent_dir()),
        std::path::Path::to_path_buf,
    );
    let paths = crate::host_daemon_paths::create_host_daemon_paths(socket, &agent_dir);
    let registered = crate::host_daemon_registration::read_host_registration(&paths)?;
    Ok(generation_refusal(
        crate::host_stop_generation::compare_stop_generation(registered.as_ref(), expected),
    ))
}
/// The confirmed generation re-read after the probes, and the only pid/instance the stop may signal: the expected
/// identity itself, never a registration read that was not compared with it.
fn rechecked_target(
    paths: &crate::host_daemon_paths::HostDaemonPaths,
    socket: &str,
    expected: &crate::host_stop_generation::HostStopGeneration,
    host: Option<&crate::host_protocol_info::HostProtocolInfo>,
) -> std::io::Result<Result<(u32, String), Option<StopHostOutcome>>> {
    use crate::host_stop_generation::StopGenerationMatch;
    let current = crate::host_daemon_registration::read_host_registration(paths)?;
    let matched = crate::host_stop_generation::compare_stop_generation(current.as_ref(), expected);
    if let Some(refusal) = generation_refusal(matched) {
        return Ok(Err(Some(refusal)));
    }
    if matched == StopGenerationMatch::Proven
        && crate::host_daemon_registration::proven_owner(current.as_ref(), socket).is_none()
    {
        return Ok(Err(None));
    }
    if matched == StopGenerationMatch::BoundWithoutStartTime {
        // No start time was recorded (Windows), so prove the same process another way before signalling: this endpoint,
        // a live process that started before it wrote this registration, and, when the host answered, the confirmed instance.
        if current
            .as_ref()
            .and_then(|current| current.socket.as_deref())
            .is_some_and(|recorded| {
                crate::host_ensure::normalize_socket_path(recorded)
                    != crate::host_ensure::normalize_socket_path(socket)
            })
        {
            return Ok(Err(None));
        }
        if !crate::host_stop_start_time::started_before_registration(
            expected.pid,
            &crate::host_daemon_paths::generation_paths(paths, &expected.instance_id).pid_file,
        ) {
            return Ok(Err(None));
        }
        if host
            .and_then(|host| host.instance_id.as_deref())
            .is_some_and(|instance| instance != expected.instance_id)
        {
            return Ok(Err(Some(StopHostOutcome::Refuse {
                reason: "stale_generation",
            })));
        }
    }
    Ok(Ok((expected.pid, expected.instance_id.clone())))
}
async fn stop_host_locked(options: StopHostOptions) -> std::io::Result<StopHostOutcome> {
    let agent_dir = options
        .agent_dir
        .clone()
        .unwrap_or_else(|| maho_core::config::get_agent_dir().into());
    let paths = crate::host_daemon_paths::create_host_daemon_paths(&options.socket, &agent_dir);
    let timeout_ms = options
        .timeout_ms
        .unwrap_or(crate::host_stop_drain::DEFAULT_DRAIN_PROBE_TIMEOUT_MS);
    let expected = options.expected_generation.as_ref();
    if let Some(expected) = expected {
        let registered = crate::host_daemon_registration::read_host_registration(&paths)?;
        if let Some(refusal) = generation_refusal(
            crate::host_stop_generation::compare_stop_generation(registered.as_ref(), expected),
        ) {
            return Ok(refusal);
        }
    }
    if options.drain {
        let result = crate::host_stop_drain::drain_host(
            &paths,
            crate::host_stop_drain::DrainHostOptions {
                socket: options.socket.clone(),
                instance_id: options.instance_id.clone(),
                wait_ms: options.wait_ms,
                timeout_ms: Some(timeout_ms),
            },
        )
        .await?;
        return Ok(StopHostOutcome::Drain(result));
    }
    let owner = if expected.is_none() {
        crate::host_daemon_registration::proven_owner(
            crate::host_daemon_registration::read_host_registration(&paths)?.as_ref(),
            &options.socket,
        )
        .map(|owner| (owner.pid, owner.instance_id.clone()))
    } else {
        None
    };
    if expected.is_none() && owner.is_none() {
        return Ok(StopHostOutcome::Refuse {
            reason: "unknown_owner",
        });
    }
    let host = crate::host_probe::probe_protocol_info(&options.socket, timeout_ms).await;
    // A hard stop ends whatever the host is doing, including work that belongs to other clients: it is allowed only
    // against a host that reports nothing open, or by an explicit override.
    let sessions = crate::host_probe::probe_session_count(&options.socket, timeout_ms).await;
    if !options.force && host.is_some() && sessions.is_some_and(|sessions| sessions > 0) {
        return Ok(StopHostOutcome::Refuse {
            reason: "sessions_live",
        });
    }
    if options.require_answer && (host.is_none() || sessions.is_none()) {
        return Ok(StopHostOutcome::Refuse {
            reason: "sessions_live",
        });
    }
    let (pid, instance_id) = match expected {
        None => match owner {
            Some(owner) => owner,
            None => {
                return Ok(StopHostOutcome::Refuse {
                    reason: "unknown_owner",
                });
            }
        },
        Some(expected) => match rechecked_target(&paths, &options.socket, expected, host.as_ref())?
        {
            Ok(target) => target,
            Err(Some(refusal)) => return Ok(refusal),
            Err(None) => {
                return Ok(StopHostOutcome::Refuse {
                    reason: "unknown_owner",
                });
            }
        },
    };
    #[cfg(unix)]
    {
        let delivered =
            signal_generation(pid, rustix::process::Signal::TERM).map_err(std::io::Error::from)?;
        // A signal is not an exit: only the supervisor may release ownership after reaping its host.
        if !delivered {
            crate::host_daemon_registration::release_generation(&paths, &instance_id, pid)?;
        }
        Ok(StopHostOutcome::Stopped { pid })
    }
    #[cfg(windows)]
    {
        let delivered = terminate_generation(pid)?;
        if !delivered {
            crate::host_daemon_registration::release_generation(&paths, &instance_id, pid)?;
        }
        Ok(StopHostOutcome::Stopped { pid })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pid, instance_id);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "stopping hosts is unsupported on this platform",
        ))
    }
}
pub fn stop_decision(
    owner_proven: bool,
    drain: bool,
    force: bool,
    handoff_capable: bool,
    host_answered: bool,
    sessions: Option<usize>,
) -> StopDecision {
    if !owner_proven {
        return StopDecision::Refuse("unknown_owner");
    }
    if drain {
        return if !handoff_capable {
            StopDecision::Refuse("drain_unsupported")
        } else {
            StopDecision::Drain
        };
    }
    if !force && host_answered && sessions.is_some_and(|sessions| sessions > 0) {
        return StopDecision::Refuse("sessions_live");
    }
    StopDecision::Stop
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn a_bound_stop_cannot_be_combined_with_drain() {
        let error = stop_host_with(StopHostOptions {
            socket: "s".into(),
            drain: true,
            expected_generation: Some(crate::host_stop_generation::HostStopGeneration {
                instance_id: "i".into(),
                generation: 0,
                pid: 1,
                process_start_time: None,
            }),
            ..Default::default()
        })
        .await
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("cannot be combined with drain"));
    }
    #[test]
    fn an_endpoint_nobody_registered_is_stale_for_a_bound_stop() {
        let temp = tempfile::tempdir().unwrap();
        let expected = crate::host_stop_generation::HostStopGeneration {
            instance_id: "i".into(),
            generation: 0,
            pid: 1,
            process_start_time: None,
        };
        assert_eq!(
            stale_stop_generation("missing", Some(temp.path()), &expected).unwrap(),
            Some(StopHostOutcome::Refuse {
                reason: "stale_generation"
            })
        );
    }
    #[test]
    fn guards_owner_before_signal_and_live_sessions_before_hard_stop() {
        assert_eq!(
            stop_decision(false, true, true, true, true, Some(1)),
            StopDecision::Refuse("unknown_owner")
        );
        assert_eq!(
            stop_decision(true, true, false, false, true, Some(0)),
            StopDecision::Refuse("drain_unsupported")
        );
        assert_eq!(
            stop_decision(true, false, false, true, true, Some(1)),
            StopDecision::Refuse("sessions_live")
        );
        assert_eq!(
            stop_decision(true, true, false, true, true, Some(1)),
            StopDecision::Drain
        );
        assert_eq!(
            stop_decision(true, false, true, true, true, Some(1)),
            StopDecision::Stop
        );
    }
    #[cfg(unix)]
    #[test]
    fn process_gone_is_already_stopped() {
        assert!(!signal_generation(0, rustix::process::Signal::TERM).unwrap());
    }
}
