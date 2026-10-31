//! The host-side wire of the conditional idle handover (senpi `host-handover-wire.ts`, plus the `begin_handover`
//! answer of `host-core-gate.ts`): the request a CLI sends, the answer it gets, and the refusal of a request that
//! names another agent dir than this host's (#3560).
//!
//! `begin_handover` is answered with `success: true` whatever the verdict, so a client reads one `data` shape:
//! `{ state, operation_id, ... }` for an operation, `{ refused, detail }` for a refusal.
use crate::host_idle_handover::{
    HandoverAnswer, HandoverLaunch, HandoverView, HostIdleHandover, IdleHandoverRequest,
};
use crate::host_launch_spec::HostLifecyclePolicyInput;
use serde_json::{Map, Value, json};

pub use crate::host_handover_request::BEGIN_HANDOVER_COMMAND;

fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    value?
        .as_array()?
        .iter()
        .map(|entry| entry.as_str().map(str::to_owned))
        .collect()
}

/// Parses a `begin_handover` record into its request, or the reason it is invalid.
pub fn parse_begin_handover(record: &Map<String, Value>) -> Result<IdleHandoverRequest, String> {
    let operation_id = record
        .get("operation_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or("operation_id must be a non-empty string")?;
    let if_instance_id = record
        .get("if_instance_id")
        .and_then(Value::as_str)
        .ok_or("if_instance_id must be a string")?;
    let if_generation = record
        .get("if_generation")
        .and_then(Value::as_f64)
        .filter(|value| value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0)
        .ok_or("if_generation must be an integer")?;
    let target_runtime_build_id = record
        .get("target_runtime_build_id")
        .and_then(Value::as_str)
        .ok_or("target_runtime_build_id must be a string")?;
    let launch = record
        .get("launch")
        .and_then(Value::as_object)
        .and_then(|launch| {
            Some(HandoverLaunch {
                command: launch.get("command")?.as_str()?.to_owned(),
                args: string_array(launch.get("args"))?,
            })
        })
        .ok_or("launch must be { command, args }")?;
    let host_args =
        string_array(record.get("host_args")).ok_or("host_args must be an array of strings")?;
    let env = record
        .get("env")
        .and_then(Value::as_object)
        .and_then(|env| {
            env.iter()
                .map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
                .collect::<Option<std::collections::BTreeMap<_, _>>>()
        })
        .ok_or("env must map names to strings")?;
    let policy = record
        .get("policy")
        .and_then(Value::as_object)
        .map(|policy| HostLifecyclePolicyInput {
            idle_exit_ms: policy.get("idleExitMs").and_then(Value::as_f64),
            cold_start: policy
                .get("coldStart")
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    let mcp_presenter_enforcement = match record.get("mcp_presenter_enforcement") {
        None => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => return Err("mcp_presenter_enforcement must be a boolean".into()),
    };
    let agent_dir = match record.get("agent_dir") {
        None => None,
        Some(Value::String(dir)) if !dir.is_empty() => Some(dir.clone()),
        Some(_) => return Err("agent_dir must be a non-empty string".into()),
    };
    Ok(IdleHandoverRequest {
        operation_id: operation_id.into(),
        if_instance_id: if_instance_id.into(),
        if_generation,
        target_runtime_build_id: target_runtime_build_id.into(),
        launch,
        host_args,
        env,
        policy,
        mcp_presenter_enforcement,
        agent_dir,
    })
}

pub fn handover_wire(view: &HandoverView) -> Value {
    let mut wire = json!({
        "operation_id": view.operation_id,
        "state": view.state.as_str(),
        "target_runtime_build_id": view.target_runtime_build_id,
    });
    if let Some(reason) = &view.reason {
        wire["reason"] = reason.clone().into();
    }
    if let Some(successor) = &view.successor {
        wire["successor"] = json!({
            "pid": successor.pid,
            "instanceId": successor.instance_id,
            "generation": successor.generation,
            "runtimeBuildId": successor.runtime_build_id,
        });
    }
    wire
}

/// The answer to one `begin_handover`; `Err` carries an invalid-request reason.
pub fn begin_handover_response(
    id: Option<&Value>,
    answer: Result<HandoverAnswer, String>,
) -> Value {
    let data = match answer {
        Err(detail) => json!({"refused":"invalid_request","detail":detail}),
        Ok(HandoverAnswer::Refused { reason, detail }) => {
            let mut data = json!({ "refused": reason });
            if let Some(detail) = detail {
                data["detail"] = detail.into();
            }
            data
        }
        Ok(HandoverAnswer::Operation(view)) => handover_wire(&view),
    };
    let mut response =
        json!({"type":"response","command":BEGIN_HANDOVER_COMMAND,"success":true,"data":data});
    if let Some(id) = id.filter(|id| id.is_string()) {
        response["id"] = id.clone();
    }
    response
}

/// What a command carrying new work gets while a handover holds it back (senpi `handoverRefusal`).
pub fn handover_refusal(id: Option<&str>, command_type: &str, view: &HandoverView) -> Value {
    let mut response = json!({
        "type": "response",
        "command": command_type,
        "success": false,
        "error": format!(
            "handover_pending: this host is handing over to runtime {} (operation {}); new work is admitted by its successor",
            view.target_runtime_build_id, view.operation_id
        ),
    });
    if let Some(id) = id {
        response["id"] = id.into();
    }
    response
}

/// `get_protocol_info` gains the runtime this host loaded and its handover operation (senpi `withHostIdentity`).
pub fn with_host_identity(
    data: &mut Value,
    runtime_build_id: Option<&str>,
    handover: Option<&HandoverView>,
) {
    if let Some(runtime_build_id) = runtime_build_id {
        data["runtimeBuildId"] = runtime_build_id.into();
    }
    if let Some(handover) = handover {
        data["handover"] = handover_wire(handover);
    }
}

/// The safe idle point (senpi `isHostIdle`): no session opening (an in-flight request without a session), and every
/// listed session open, without an in-flight request and not busy by its activity snapshot - the same per-session
/// judgement a drain parks by. A retained session with no work is idle.
pub fn is_host_idle<'a>(
    candidates: Vec<crate::session_drain::DrainCandidate<'a>>,
    sessionless_in_flight: bool,
    in_flight: impl Fn(&str) -> bool,
) -> bool {
    if sessionless_in_flight {
        return false;
    }
    let sessions = candidates.len();
    let verdicts = crate::session_drain::select_drain_verdicts(candidates, in_flight, |_, _| {});
    verdicts.park.len() + verdicts.gone.len() == sessions
}

/// This host's agent dir and public socket: a `begin_handover` naming another agent dir is refused (#3560).
#[derive(Debug, Clone)]
pub struct HostEndpoint {
    pub agent_dir: std::path::PathBuf,
    pub socket: String,
}

/// senpi `HostCoreGate.answerBegin`: parse, refuse a foreign agent dir, then hand the request to the operation.
pub async fn answer_begin_handover(
    handover: Option<&HostIdleHandover>,
    endpoint: Option<&HostEndpoint>,
    record: &Map<String, Value>,
) -> Value {
    let id = record.get("id");
    let Some(handover) = handover else {
        return begin_handover_response(
            id,
            Ok(HandoverAnswer::Refused {
                reason: "handover_unsupported",
                detail: None,
            }),
        );
    };
    let request = match parse_begin_handover(record) {
        Ok(request) => request,
        Err(detail) => return begin_handover_response(id, Err(detail)),
    };
    // Settled before `begin` so its claim on the operation stays free of awaits.
    let detail = match (&request.agent_dir, endpoint) {
        (Some(agent_dir), Some(endpoint)) => crate::host_handoff_agent_dir::agent_dir_mismatch(
            &endpoint.socket,
            std::path::Path::new(agent_dir),
            &endpoint.agent_dir,
        ),
        _ => None,
    };
    let answer = match detail {
        Some(detail) => HandoverAnswer::Refused {
            reason: crate::host_handoff_agent_dir::FOREIGN_AGENT_DIR,
            detail: Some(detail),
        },
        None => handover.begin(request).await,
    };
    begin_handover_response(id, Ok(answer))
}

const SUCCESSOR_PROBE_TIMEOUT_MS: u64 = 10_000;

/// Runs the handover the host decided on (senpi `performIdleHandover`): the ordinary generation handoff, launched
/// from the runtime of the CLI that asked, with that CLI's daemon environment. The successor is told the runtime
/// it must report (`EXPECTED_RUNTIME_BUILD_ID_ENV`) and exits before it listens when it computes another one. The
/// env is applied EXACTLY: every name this host has and the CLI did not send is removed; the successor's identity
/// is forced after it (`host_handoff::successor_launch_env`). `presenter_bootstrap` is
/// the running host's successor enrollment channel (`HostCore::presenter_successor_bootstrap`).
pub async fn perform_idle_handover(
    request: IdleHandoverRequest,
    socket: &str,
    agent_dir: &std::path::Path,
    presenter_bootstrap: Option<crate::mcp_presenter_wire::BoxedPrivateStream>,
) -> crate::host_idle_handover::HandoverOutcome {
    let mut env: std::collections::HashMap<String, Option<String>> =
        std::env::vars().map(|(name, _)| (name, None)).collect();
    for (name, value) in &request.env {
        env.insert(name.clone(), Some(value.clone()));
    }
    let policy = request
        .policy
        .as_ref()
        .map(|policy| crate::host_lifecycle::HostLifecyclePolicy {
            cold_start: policy
                .cold_start
                .clone()
                .unwrap_or_else(|| "transient".into()),
            idle_exit_ms: policy
                .idle_exit_ms
                .unwrap_or(crate::host_lifecycle::DEFAULT_HOST_IDLE_EXIT_MS),
        });
    let options = crate::host_handoff::HandoffOptions {
        // Already the supervisor argv of the CLI that asked (`ensure_supervisor_args`), passed through as upstream.
        host_args: request.host_args.clone(),
        policy,
        env,
        expected_runtime_build_id: Some(request.target_runtime_build_id.clone()),
        launch_profile_id: String::new(),
        readiness_ms: crate::host_successor::DEFAULT_HANDOFF_READINESS_MS,
        launch: Some(crate::host_launch::HostLaunch {
            command: request.launch.command.clone().into(),
            args: request.launch.args.clone(),
        }),
    };
    let presenter = crate::mcp_presenter_bootstrap::PresenterLaunchOptions {
        bootstrap: std::sync::Mutex::new(presenter_bootstrap),
        enforcement: request.mcp_presenter_enforcement.unwrap_or(false),
    };
    let result = crate::mcp_presenter_bootstrap::with_presenter_launch(
        presenter,
        crate::host_handoff::handoff_host(socket, agent_dir, options),
    )
    .await;
    let successor = match result {
        Err(error) => return Err(format!("successor_unavailable: {error}")),
        Ok(Err(refusal)) => {
            return Err(match refusal.detail {
                Some(detail) => format!("{}: {detail}", refusal.reason),
                None => refusal.reason.to_owned(),
            });
        }
        Ok(Ok(successor)) => successor,
    };
    // Bookkeeping after the socket moved: a failed write must not report a live successor as blocked.
    let mut keys = request.env.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    if let Err(cause) = crate::host_daemon_env::write_daemon_env_keys(
        &crate::host_daemon_paths::create_host_daemon_paths(socket, agent_dir),
        &keys,
    ) {
        eprintln!("senpi rpc handover: could not record the daemon environment: {cause}");
    }
    let answered = crate::host_probe::request_on_socket(
        socket,
        &json!({"type": "get_protocol_info"}),
        SUCCESSOR_PROBE_TIMEOUT_MS,
    )
    .await;
    Ok(crate::host_idle_handover::HandoverSuccessor {
        pid: successor.child.id().unwrap_or_default(),
        instance_id: successor.protocol.instance_id.clone().unwrap_or_default(),
        generation: successor.protocol.generation.unwrap_or(0.),
        runtime_build_id: answered
            .as_ref()
            .and_then(|data| {
                crate::runtime_build_id::parse_runtime_build_id(&data["runtimeBuildId"])
            })
            .map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(extra: Value) -> Map<String, Value> {
        let mut base = json!({"type":"begin_handover","id":"r","operation_id":"op","if_instance_id":"i","if_generation":1,"target_runtime_build_id":"new","launch":{"command":"senpi","args":[]},"host_args":[],"env":{"A":"1"}});
        for (key, value) in extra.as_object().unwrap() {
            base[key] = value.clone();
        }
        base.as_object().unwrap().clone()
    }
    #[test]
    fn agent_dir_and_enforcement_are_validated() {
        let parsed = parse_begin_handover(&record(
            json!({"agent_dir":"/a","mcp_presenter_enforcement":true}),
        ))
        .unwrap();
        assert_eq!(parsed.agent_dir.as_deref(), Some("/a"));
        assert_eq!(parsed.mcp_presenter_enforcement, Some(true));
        assert_eq!(
            parse_begin_handover(&record(json!({"agent_dir":""}))).unwrap_err(),
            "agent_dir must be a non-empty string"
        );
        assert_eq!(
            parse_begin_handover(&record(json!({"mcp_presenter_enforcement":"yes"}))).unwrap_err(),
            "mcp_presenter_enforcement must be a boolean"
        );
    }
    #[test]
    fn pending_work_refusal_and_identity_fields() {
        let view = HandoverView {
            operation_id: "op".into(),
            state: crate::host_idle_handover::HandoverState::Pending,
            target_runtime_build_id: "sha256:x".into(),
            reason: None,
            successor: None,
        };
        let refused = handover_refusal(Some("1"), "prompt", &view);
        assert_eq!(refused["id"], "1");
        assert_eq!(refused["success"], false);
        assert_eq!(
            refused["error"],
            "handover_pending: this host is handing over to runtime sha256:x (operation op); new work is admitted by its successor"
        );
        let mut data = json!({"serverVersion": "1"});
        with_host_identity(&mut data, Some("sha256:y"), Some(&view));
        assert_eq!(data["runtimeBuildId"], "sha256:y");
        assert_eq!(data["handover"]["state"], "handover_pending");
        let mut bare = json!({});
        with_host_identity(&mut bare, None, None);
        assert_eq!(bare, json!({}));
    }
    #[test]
    fn idle_needs_no_sessionless_request_and_every_session_settled() {
        let candidate = |id| crate::session_drain::DrainCandidate {
            session_id: id,
            session_path: None,
            worker_handoff_busy: false,
            activity: None,
        };
        assert!(is_host_idle(vec![], false, |_| false));
        assert!(!is_host_idle(vec![], true, |_| false));
        assert!(is_host_idle(
            vec![candidate("a"), candidate("b")],
            false,
            |_| false
        ));
        assert!(!is_host_idle(
            vec![candidate("a"), candidate("b")],
            false,
            |id| id == "b"
        ));
        let busy = crate::session_drain::DrainCandidate {
            worker_handoff_busy: true,
            ..candidate("w")
        };
        assert!(!is_host_idle(vec![busy], false, |_| false));
    }
    #[tokio::test]
    async fn a_request_from_another_agent_dir_is_refused_before_the_operation() {
        let temp = tempfile::tempdir().unwrap();
        let ours = temp.path().join("ours");
        std::fs::create_dir_all(&ours).unwrap();
        let handover = HostIdleHandover::new(crate::host_idle_handover::HostIdleHandoverOptions {
            identity: Box::new(|| crate::host_idle_handover::HostIdentityView {
                instance_id: "i".into(),
                generation: 1.,
                runtime_build_id: None,
            }),
            is_idle: Box::new(|| false),
            perform: Box::new(|_| Box::pin(async { Err("never".to_owned()) })),
            poll_ms: Some(3_600_000),
            log: None,
        });
        let endpoint = HostEndpoint {
            agent_dir: ours,
            socket: "/s.sock".into(),
        };
        let other = temp.path().join("theirs").to_string_lossy().into_owned();
        let answer = answer_begin_handover(
            Some(&handover),
            Some(&endpoint),
            &record(json!({ "agent_dir": other })),
        )
        .await;
        assert_eq!(answer["data"]["refused"], "foreign_agent_dir");
        assert_eq!(answer["id"], "r");
        assert!(handover.view().is_none());
        let unsupported = answer_begin_handover(None, None, &record(json!({}))).await;
        assert_eq!(unsupported["data"]["refused"], "handover_unsupported");
    }
}
