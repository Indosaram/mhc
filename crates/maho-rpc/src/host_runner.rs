use crate::host_protocol_info::HostProtocolInfo;
pub const HOST_EXIT_OK: i32 = 0;
pub const HOST_EXIT_ERROR: i32 = 1;
pub const HOST_EXIT_USAGE: i32 = 2;
pub const HOST_EXIT_REFUSED: i32 = 3;
pub const HOST_EXIT_FALLBACK: i32 = 4;
pub const HOST_PROBE_TIMEOUT_MS: u64 = 10_000;
/// Which daemon a request is about: the endpoint, and the agent directory holding its state.
pub struct HostTarget {
    pub socket: String,
    pub agent_dir: std::path::PathBuf,
}
pub struct HostOutcome {
    pub exit_code: i32,
    pub payload: serde_json::Value,
}
pub struct EnsureRequest {
    pub client: crate::host_decision::HostDecisionClient,
    pub policy: crate::host_decision::HostDecisionPolicy,
    pub launch: crate::host_launch::HostLaunch,
    pub start: crate::host_ensure::HostStartOptions,
    pub handoff: crate::host_handoff::HandoffOptions,
    pub env_keys: Vec<String>,
}
/// One host request, performed: ensure a daemon, report one, stop one, or hand one off.
pub enum HostRequest {
    Ensure {
        target: HostTarget,
        request: Box<EnsureRequest>,
    },
    Status {
        target: HostTarget,
        include_workers: bool,
        /// Every endpoint under `target.agent_dir` (its socket is ignored), read without pruning.
        all: bool,
    },
    Stop {
        target: HostTarget,
        drain: bool,
        force: bool,
        /// The registration the operator saw and authorized; a stop refuses with `stale_generation` before probing when it no longer matches.
        expected_generation: Option<HostStopGeneration>,
        /// With `drain`: the generation to drain, by instance id (senpi#3653).
        instance_id: Option<String>,
        /// With `drain`: wait up to this long for that generation to exit.
        wait_ms: Option<u64>,
    },
    Handoff {
        target: HostTarget,
        options: crate::host_handoff::HandoffOptions,
        env_keys: Vec<String>,
        /// The conditional form (`--when idle`): the running host hands over at its next safe idle point.
        handover: Option<crate::host_handover_request::IdleHandoverTerms>,
        /// The runtime build id of the process that would launch the successor; `None` when unreadable.
        client_runtime_build_id: Option<String>,
    },
    /// Removes endpoint state under `agent_dir` only on the three-part evidence (`host_gc.rs`).
    Gc { agent_dir: std::path::PathBuf },
    /// The owner-keyed shard naming contract; contacts no host.
    ShardPath {
        kind: crate::host_endpoints::ShardKind,
        owner: String,
        root: std::path::PathBuf,
    },
}
pub async fn run_host_request(request: HostRequest) -> std::io::Result<HostOutcome> {
    // A terminal control endpoint belongs to its terminal: ensure, handoff and stop refuse from disk alone.
    let guarded = match &request {
        HostRequest::Ensure { target, .. }
        | HostRequest::Stop { target, .. }
        | HostRequest::Handoff { target, .. } => Some(target),
        _ => None,
    };
    if let Some(target) = guarded
        && crate::host_endpoints::endpoint_kind_of_socket(&target.socket, &target.agent_dir)
            == crate::host_endpoints::EndpointKind::Tui
    {
        return Ok(refusal(
            false,
            None,
            serde_json::Map::from_iter([
                ("reason".into(), "unsupported_endpoint_kind".into()),
                ("socket".into(), target.socket.clone().into()),
                ("endpoint_kind".into(), "tui".into()),
            ]),
        ));
    }
    match request {
        HostRequest::Ensure { target, request } => {
            ensure_outcome(&target.socket, &target.agent_dir, *request).await
        }
        HostRequest::Status {
            target,
            include_workers,
            all,
        } => {
            if all {
                status_all_outcome(&target.agent_dir, include_workers).await
            } else {
                status_outcome(&target.socket, &target.agent_dir, include_workers).await
            }
        }
        HostRequest::Stop {
            target,
            drain,
            force,
            expected_generation,
            instance_id,
            wait_ms,
        } => {
            stop_outcome_with(
                &target.socket,
                &target.agent_dir,
                StopOptions {
                    drain,
                    force,
                    expected_generation,
                    instance_id,
                    wait_ms,
                },
            )
            .await
        }
        HostRequest::Handoff {
            target,
            options,
            env_keys,
            handover,
            client_runtime_build_id,
        } => match handover {
            None => handoff_outcome(&target.socket, &target.agent_dir, options, &env_keys).await,
            Some(terms) => {
                crate::host_handover_request::idle_handover_outcome(
                    &target.socket,
                    &target.agent_dir,
                    &options,
                    &terms,
                    client_runtime_build_id.as_deref(),
                )
                .await
            }
        },
        HostRequest::Gc { agent_dir } => Ok(HostOutcome {
            exit_code: HOST_EXIT_OK,
            payload: crate::host_gc::gc_host_endpoints(&agent_dir, &Default::default())
                .await
                .to_value(),
        }),
        HostRequest::ShardPath { kind, owner, root } => Ok(shard_path_outcome(kind, &owner, &root)),
    }
}
pub async fn ensure_outcome(
    socket: &str,
    agent_dir: &std::path::Path,
    request: EnsureRequest,
) -> std::io::Result<HostOutcome> {
    let before = crate::host_probe::probe_protocol_info(socket, 10000).await;
    if request.policy == crate::host_decision::HostDecisionPolicy::Fallback
        && let crate::host_decision::HostDecision::Fallback { reason, .. } =
            crate::host_decision::decide_host_action(
                &request.client,
                before.as_ref(),
                request.policy,
            )
    {
        let reason = match reason {
            crate::host_decision::FallbackReason::Capability => "capability",
            crate::host_decision::FallbackReason::EngineMismatch => "engine_mismatch",
        };
        return Ok(refusal(
            true,
            before.as_ref(),
            serde_json::Map::from_iter([
                ("reason".into(), reason.into()),
                ("socket".into(), socket.into()),
            ]),
        ));
    }
    let prepared =
        crate::host_ensure::prepare_ensure_host(socket, agent_dir, request.client, request.policy)
            .await
            .map_err(std::io::Error::other)?;
    let ensured = crate::host_ensure::ensure_prepared_host(
        prepared,
        socket,
        agent_dir,
        &request.launch,
        request.start,
        request.handoff,
    )
    .await?;
    let (pid, reused) = match ensured {
        crate::host_ensure::EnsuredHost::Refused(reason) => {
            return Ok(refusal(
                false,
                before.as_ref(),
                serde_json::Map::from_iter([
                    ("reason".into(), reason.into()),
                    ("socket".into(), socket.into()),
                ]),
            ));
        }
        crate::host_ensure::EnsuredHost::ForeignWriter(holder) => {
            let error = crate::host_decision::HostEnsureRefusedError::new(
                socket,
                crate::host_decision::HostRefusalReason::ForeignWriter,
                None,
            )
            .with_holder(holder);
            return Ok(refusal(
                false,
                before.as_ref(),
                ensure_refusal_body(socket, &error),
            ));
        }
        crate::host_ensure::EnsuredHost::Reused { pid } => (pid, true),
        crate::host_ensure::EnsuredHost::Started(host) => {
            (host.child.id().unwrap_or_default(), false)
        }
        crate::host_ensure::EnsuredHost::HandedOff(host) => {
            (host.child.id().unwrap_or_default(), false)
        }
    };
    let host = crate::host_probe::probe_protocol_info(socket, 10000).await;
    if !reused {
        let paths = crate::host_daemon_paths::create_host_daemon_paths(socket, agent_dir);
        crate::host_daemon_env::write_daemon_env_keys(&paths, &request.env_keys)
            .map_err(|error| error.source)?;
    }
    let mut payload = identity_payload(socket, pid, host.as_ref());
    payload["action"] = ensure_action(reused, before.as_ref(), host.as_ref()).into();
    payload["reused"] = reused.into();
    Ok(HostOutcome {
        exit_code: HOST_EXIT_OK,
        payload,
    })
}
pub async fn handoff_outcome(
    socket: &str,
    agent_dir: &std::path::Path,
    options: crate::host_handoff::HandoffOptions,
    env_keys: &[String],
) -> std::io::Result<HostOutcome> {
    let before = crate::host_probe::probe_protocol_info(socket, 10000).await;
    match crate::host_handoff::handoff_host(socket, agent_dir, options).await? {
        Err(result) => Ok(refusal(
            false,
            before.as_ref(),
            serde_json::Map::from_iter([
                ("reason".into(), upgrade_refusal(result.reason).into()),
                ("socket".into(), socket.into()),
                ("detail".into(), result.reason.into()),
                ("upgradeable".into(), result.upgradeable.into()),
            ]),
        )),
        Ok(successor) => {
            let paths = crate::host_daemon_paths::create_host_daemon_paths(socket, agent_dir);
            crate::host_daemon_env::write_daemon_env_keys(&paths, env_keys)
                .map_err(|error| error.source)?;
            let pid = successor.child.id().unwrap_or_default();
            let mut payload = identity_payload(socket, pid, Some(&successor.protocol));
            payload["action"] = "handoff".into();
            payload["reused"] = false.into();
            Ok(HostOutcome {
                exit_code: HOST_EXIT_OK,
                payload,
            })
        }
    }
}
/// The registration the operator saw and authorized (senpi `HostStopGeneration`), shared with `host_stop`.
pub use crate::host_stop_generation::{HostStopGeneration, StopGenerationMatch, compare_stop_generation};
/// The refusal reason for a binding the endpoint no longer matches; `None` when proven or bound (senpi `generationRefusal`).
pub fn generation_refusal(matched: StopGenerationMatch) -> Option<&'static str> {
    match matched {
        StopGenerationMatch::Stale => Some("stale_generation"),
        StopGenerationMatch::Unprovable => Some("unknown_owner"),
        _ => None,
    }
}
/// Refuses a binding the endpoint no longer matches, before any probe or census (senpi `staleStopGeneration`).
pub fn stale_stop_generation(
    socket: &str,
    agent_dir: &std::path::Path,
    expected: &HostStopGeneration,
) -> std::io::Result<Option<&'static str>> {
    let paths = crate::host_daemon_paths::create_host_daemon_paths(socket, agent_dir);
    let registered = crate::host_daemon_registration::read_host_registration(&paths)?;
    Ok(generation_refusal(compare_stop_generation(
        registered.as_ref(),
        expected,
    )))
}
/// The body of an ensure refusal raised as a `HostEnsureRefusedError`: carries `holder` when the error has one.
/// (The Rust error has no caller-supplied detail, so no `detail` key; the standard wording lives in the message.)
pub fn ensure_refusal_body(
    socket: &str,
    error: &crate::host_decision::HostEnsureRefusedError,
) -> serde_json::Map<String, serde_json::Value> {
    let mut body = serde_json::Map::from_iter([
        ("reason".into(), error.reason.as_str().into()),
        ("socket".into(), socket.into()),
    ]);
    if let Some(holder) = &error.holder
        && let Ok(value) = serde_json::to_value(holder)
    {
        body.insert("holder".into(), value);
    }
    body
}
pub fn identity_payload(
    socket: &str,
    pid: u32,
    host: Option<&HostProtocolInfo>,
) -> serde_json::Value {
    let client = crate::host_decision::HostDecisionClient {
        protocol_version: crate::host_decision::HOST_PROTOCOL_VERSION,
        required_capabilities: crate::host_decision::REQUIRED_HOST_CAPABILITIES
            .iter()
            .map(|value| (*value).into())
            .collect(),
        identity: maho_core::engine_build_identity::engine_build_identity().clone(),
        launch_profile: None,
        started_by_us: false,
        platform: if cfg!(windows) {
            "win32"
        } else {
            std::env::consts::OS
        }
        .into(),
    };
    let decision = serde_json::to_value(crate::host_decision::decide_host_action(
        &client,
        host,
        crate::host_decision::HostDecisionPolicy::Upgrade,
    ))
    .expect("decision has no failing serializer");
    serde_json::json!({"socket":socket,"pid":pid,"instanceId":host.and_then(|host|host.instance_id.as_ref()),"generation":host.and_then(|host|host.generation),"engineVersion":host.and_then(|host|host.engine_version.as_ref()),"engineOrdinal":host.and_then(|host|host.engine_ordinal),"capabilities":host.map_or(&[][..],|host|host.capabilities.as_slice()),"launchProfileId":host.and_then(|host|host.launch_profile.as_ref()).map(|profile|&profile.profile_id),"upgradeable":decision["upgradeable"]})
}
pub async fn status_outcome(
    socket: &str,
    agent_dir: &std::path::Path,
    include_workers: bool,
) -> std::io::Result<HostOutcome> {
    let payload = crate::host_status::read_host_status(socket, agent_dir, include_workers).await?;
    Ok(HostOutcome {
        exit_code: if payload["reachable"] == true {
            HOST_EXIT_OK
        } else {
            HOST_EXIT_REFUSED
        },
        payload,
    })
}
/// `status --all`: one row per endpoint under the agent directory; exit 0 whatever the rows say.
pub async fn status_all_outcome(
    agent_dir: &std::path::Path,
    include_workers: bool,
) -> std::io::Result<HostOutcome> {
    let rows =
        crate::host_status_all::read_all_host_status(agent_dir, include_workers, None).await?;
    Ok(HostOutcome {
        exit_code: HOST_EXIT_OK,
        payload: serde_json::Value::Array(rows),
    })
}
pub fn shard_path_outcome(
    kind: crate::host_endpoints::ShardKind,
    owner: &str,
    root: &std::path::Path,
) -> HostOutcome {
    let key = crate::host_endpoints::shard_key(kind, owner);
    HostOutcome {
        exit_code: HOST_EXIT_OK,
        payload: serde_json::json!({"kind":kind.as_str(),"key":key,"socket":crate::host_endpoints::shard_socket_path_for_key(root,kind,&key)}),
    }
}
pub async fn stop_outcome(
    socket: &str,
    agent_dir: &std::path::Path,
    drain: bool,
    force: bool,
) -> std::io::Result<HostOutcome> {
    stop_outcome_with(
        socket,
        agent_dir,
        StopOptions {
            drain,
            force,
            ..Default::default()
        },
    )
    .await
}
#[derive(Default)]
pub struct StopOptions {
    pub drain: bool,
    pub force: bool,
    pub expected_generation: Option<HostStopGeneration>,
    pub instance_id: Option<String>,
    pub wait_ms: Option<u64>,
}
pub async fn stop_outcome_with(
    socket: &str,
    agent_dir: &std::path::Path,
    options: StopOptions,
) -> std::io::Result<HostOutcome> {
    let StopOptions {
        drain,
        force,
        expected_generation,
        instance_id,
        wait_ms,
    } = options;
    if let Some(expected) = &expected_generation
        && let Some(reason) = stale_stop_generation(socket, agent_dir, expected)?
    {
        return Ok(refusal(
            false,
            None,
            serde_json::Map::from_iter([
                ("reason".into(), reason.into()),
                ("socket".into(), socket.into()),
            ]),
        ));
    }
    let host = crate::host_probe::probe_protocol_info(socket, 10000).await;
    let sessions = crate::host_status::read_session_counts(socket, true).await;
    let reason = if !drain && !force && sessions.foreign_attached + sessions.foreign_retained > 0 {
        Some("sessions_live")
    } else {
        let outcome = crate::host_stop::stop_host_with(crate::host_stop::StopHostOptions {
            socket: socket.into(),
            agent_dir: Some(agent_dir.into()),
            drain,
            instance_id,
            wait_ms,
            force,
            timeout_ms: Some(HOST_PROBE_TIMEOUT_MS),
            expected_generation: if drain { None } else { expected_generation },
            ..Default::default()
        })
        .await?;
        match outcome {
            crate::host_stop::StopHostOutcome::Refuse { reason }
            | crate::host_stop::StopHostOutcome::Drain(
                crate::host_stop_drain::DrainHostResult::Refuse { reason },
            ) => Some(reason),
            crate::host_stop::StopHostOutcome::Drain(
                crate::host_stop_drain::DrainHostResult::Drained {
                    pid,
                    instance_id,
                    exited,
                },
            ) => {
                let mut payload = serde_json::json!({"action":"drained","socket":socket,"pid":pid,"sessions":sessions});
                if let Some(instance_id) = instance_id {
                    payload["instanceId"] = instance_id.into();
                }
                if let Some(exited) = exited {
                    payload["exited"] = exited.into();
                }
                return Ok(HostOutcome {
                    exit_code: HOST_EXIT_OK,
                    payload,
                });
            }
            crate::host_stop::StopHostOutcome::Stopped { pid } => {
                return Ok(HostOutcome {
                    exit_code: HOST_EXIT_OK,
                    payload: serde_json::json!({"action":"stopped","socket":socket,"pid":pid,"sessions":sessions}),
                });
            }
        }
    };
    Ok(refusal(
        false,
        host.as_ref(),
        serde_json::Map::from_iter([
            ("reason".into(), reason.into()),
            ("socket".into(), socket.into()),
            (
                "sessions".into(),
                serde_json::to_value(sessions).map_err(std::io::Error::other)?,
            ),
        ]),
    ))
}
pub fn ensure_action(
    reused: bool,
    before: Option<&HostProtocolInfo>,
    after: Option<&HostProtocolInfo>,
) -> &'static str {
    if reused {
        "reuse"
    } else if before.is_some_and(|before| {
        before.instance_id.as_ref() != after.and_then(|after| after.instance_id.as_ref())
    }) {
        "handoff"
    } else {
        "start"
    }
}
pub fn upgrade_refusal(reason: &str) -> &str {
    if reason == "handoff_unsupported" {
        "upgrade_unsupported"
    } else {
        reason
    }
}
pub fn refusal(
    fallback: bool,
    host: Option<&HostProtocolInfo>,
    mut body: serde_json::Map<String, serde_json::Value>,
) -> HostOutcome {
    let kind = if fallback { "fallback" } else { "refuse" };
    let mut payload = serde_json::Map::new();
    payload.insert("action".into(), kind.into());
    payload.append(&mut body);
    payload.insert("host".into(), crate::host_status::host_summary(host));
    HostOutcome {
        exit_code: if fallback {
            HOST_EXIT_FALLBACK
        } else {
            HOST_EXIT_REFUSED
        },
        payload: payload.into(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn status_request_reports_an_absent_host_as_refused() {
        let temp = tempfile::tempdir().unwrap();
        let outcome = run_host_request(HostRequest::Status {
            target: HostTarget {
                socket: temp
                    .path()
                    .join("absent.sock")
                    .to_string_lossy()
                    .into_owned(),
                agent_dir: temp.path().to_path_buf(),
            },
            include_workers: false,
            all: false,
        })
        .await
        .unwrap();
        assert_eq!(outcome.exit_code, HOST_EXIT_REFUSED);
        assert_eq!(outcome.payload["reachable"], false);
        assert!(outcome.payload.get("action").is_none());
    }
    #[test]
    fn ensure_labels_distinguish_identity_replacement_from_start() {
        let mut before = crate::host_protocol_info::parse_host_protocol_info(
            &serde_json::json!({"serverVersion":"1","capabilities":[]}),
        )
        .unwrap();
        assert_eq!(ensure_action(false, None, None), "start");
        assert_eq!(ensure_action(false, Some(&before), None), "start");
        before.instance_id = Some("old".into());
        assert_eq!(ensure_action(false, Some(&before), None), "handoff");
        assert_eq!(ensure_action(true, Some(&before), None), "reuse");
        assert_eq!(ensure_action(false, Some(&before), Some(&before)), "start");
    }
    #[test]
    fn stop_binding_compares_instance_generation_pid_and_start_time() {
        let registered = crate::host_daemon_registration::RegisteredHost {
            pid: 7,
            process_start_time: Some("t".into()),
            socket: None,
            instance_id: "i".into(),
            generation: 2.,
            writer: None,
        };
        let expected = HostStopGeneration {
            instance_id: "i".into(),
            generation: 2,
            pid: 7,
            process_start_time: Some("t".into()),
        };
        assert_eq!(
            compare_stop_generation(Some(&registered), &expected),
            StopGenerationMatch::Proven
        );
        assert_eq!(
            compare_stop_generation(None, &expected),
            StopGenerationMatch::Stale
        );
        assert_eq!(
            compare_stop_generation(
                Some(&registered),
                &HostStopGeneration {
                    pid: 8,
                    ..expected.clone()
                }
            ),
            StopGenerationMatch::Stale
        );
        assert_eq!(
            compare_stop_generation(
                Some(&registered),
                &HostStopGeneration {
                    process_start_time: Some("u".into()),
                    ..expected.clone()
                }
            ),
            StopGenerationMatch::Stale
        );
        assert_eq!(
            compare_stop_generation(
                Some(&registered),
                &HostStopGeneration {
                    process_start_time: None,
                    ..expected.clone()
                }
            ),
            StopGenerationMatch::Unprovable
        );
        assert_eq!(
            generation_refusal(StopGenerationMatch::Stale),
            Some("stale_generation")
        );
        assert_eq!(
            generation_refusal(StopGenerationMatch::Unprovable),
            Some("unknown_owner")
        );
        assert_eq!(generation_refusal(StopGenerationMatch::Proven), None);
    }
    #[tokio::test]
    async fn stale_expected_generation_refuses_before_probing() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("absent.sock");
        let outcome = stop_outcome_with(
            socket.to_str().unwrap(),
            temp.path(),
            StopOptions {
                expected_generation: Some(HostStopGeneration {
                    instance_id: "i".into(),
                    generation: 1,
                    pid: 7,
                    process_start_time: None,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome.exit_code, HOST_EXIT_REFUSED);
        assert_eq!(outcome.payload["reason"], "stale_generation");
    }
    #[test]
    fn ensure_refusal_body_carries_the_holder() {
        let holder = crate::host_holder::HostHolder {
            pid: 4,
            instance_id: "i".into(),
            generation: 1,
            process_start_time: None,
            engine_version: None,
            launch_profile_id: None,
            writer_pid: None,
        };
        let plain = crate::host_decision::HostEnsureRefusedError::new(
            "/s",
            crate::host_decision::HostRefusalReason::ForeignWriter,
            None,
        );
        assert!(ensure_refusal_body("/s", &plain).get("holder").is_none());
        let body = ensure_refusal_body("/s", &plain.with_holder(holder));
        assert_eq!(body["holder"]["pid"], 4);
        assert_eq!(body["reason"], "foreign_writer");
    }
    #[test]
    fn refusal_preserves_body_and_exit_code() {
        let outcome = refusal(
            true,
            None,
            serde_json::Map::from_iter([("reason".into(), "missing".into())]),
        );
        assert_eq!(outcome.exit_code, 4);
        assert_eq!(outcome.payload["action"], "fallback");
        assert!(outcome.payload["host"].is_null());
        assert_eq!(
            upgrade_refusal("handoff_unsupported"),
            "upgrade_unsupported"
        );
        assert_eq!(upgrade_refusal("unknown_owner"), "unknown_owner");
    }
}
