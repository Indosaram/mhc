//! `host handoff --when idle ...`: the client half of the conditional idle handover (senpi `host-handover-request.ts`).
//!
//! The CLI may only ask for its OWN runtime, so `--target-build` must equal the runtime build id the caller
//! computed (`target_build_mismatch` otherwise; a caller that cannot read its runtime id passes `None` and is
//! always refused). It checks what it can before asking - the socket's host is the named generation and
//! advertises the handover - then hands the operation to the HOST (`begin_handover`), which owns it from there
//! (`host_idle_handover.rs`).
//!
//! Answers, exit 0: `handover_completed`, `handover_pending`. Exit 3: `stale_generation`, `handover_blocked`,
//! `handover_in_progress`, `target_build_mismatch`, `handover_unsupported`, `no_host`, `upgrade_unsupported`.
//! Exit 1 `handover_reply_lost`: the host did not answer and the socket does not serve the target.
use crate::{
    host_protocol_info::HostProtocolInfo,
    host_runner::{HOST_EXIT_ERROR, HOST_EXIT_OK, HostOutcome, identity_payload, refusal},
};
use serde_json::{Value, json};

pub const BEGIN_HANDOVER_COMMAND: &str = "begin_handover";
/// The capability a host advertises when it implements the runtime-identity handover.
pub const RUNTIME_IDENTITY_HANDOVER_CAPABILITY: &str = "runtime_identity_handover";
const PROBE_TIMEOUT_MS: u64 = 10_000;
/// 10 s probe + 2 x 10 s session count + successor start budget (10 s + 30 s readiness + 2 s + 5 s exit wait).
pub const HANDOFF_LOCK_HOLD_MS: u64 =
    PROBE_TIMEOUT_MS + 2 * 10_000 + (10_000 + 30_000 + 2_000 + 5_000);
/// A host idle at once hands over before answering: its ensure-lock wait plus the handoff itself.
pub const BEGIN_HANDOVER_TIMEOUT_MS: u64 = 2 * HANDOFF_LOCK_HOLD_MS + 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleHandoverTerms {
    pub operation_id: String,
    pub if_instance_id: String,
    pub if_generation: u64,
    pub target_runtime_build_id: String,
}

async fn probe(socket: &str) -> (Option<HostProtocolInfo>, Option<String>) {
    let data = crate::host_probe::request_on_socket(
        socket,
        &json!({"type":"get_protocol_info"}),
        PROBE_TIMEOUT_MS,
    )
    .await;
    let build = data
        .as_ref()
        .and_then(|data| data.get("runtimeBuildId"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    (
        data.as_ref()
            .and_then(crate::host_protocol_info::parse_host_protocol_info),
        build,
    )
}
/// The supervisor pid the daemon directory names as serving the socket; 0 when none is recorded.
fn serving_pid(socket: &str, agent_dir: &std::path::Path) -> u32 {
    let paths = crate::host_daemon_paths::create_host_daemon_paths(socket, agent_dir);
    crate::host_daemon_registration::read_host_registration(&paths)
        .ok()
        .flatten()
        .map_or(0, |registered| registered.pid)
}
fn begin_handover_command(
    options: &crate::host_handoff::HandoffOptions,
    terms: &IdleHandoverTerms,
) -> std::io::Result<Value> {
    let platform = if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    };
    let mut env = std::collections::BTreeMap::<String, String>::new();
    for (name, value) in std::env::vars() {
        if crate::host_daemon_env::daemon_env_is_allowed(&name, platform) {
            env.insert(name, value);
        }
    }
    for (name, value) in &options.env {
        if !crate::host_daemon_env::daemon_env_is_allowed(name, platform) {
            continue;
        }
        match value {
            Some(value) => {
                env.insert(name.clone(), value.clone());
            }
            None => {
                env.remove(name);
            }
        }
    }
    let launch = crate::host_launch::default_host_launch(&[])?;
    let mut command = json!({"type":BEGIN_HANDOVER_COMMAND,"operation_id":terms.operation_id,"if_instance_id":terms.if_instance_id,"if_generation":terms.if_generation,"target_runtime_build_id":terms.target_runtime_build_id,"launch":{"command":launch.command,"args":launch.args},"host_args":options.host_args,"env":env});
    if let Some(policy) = &options.policy {
        command["policy"] = json!({"coldStart":policy.cold_start,"idleExitMs":policy.idle_exit_ms});
    }
    Ok(command)
}
async fn answered(
    socket: &str,
    agent_dir: &std::path::Path,
    action: &str,
    host: Option<&HostProtocolInfo>,
    terms: &IdleHandoverTerms,
    handover: Option<&Value>,
) -> HostOutcome {
    let mut payload = identity_payload(socket, serving_pid(socket, agent_dir), host);
    payload["action"] = action.into();
    payload["operationId"] = handover
        .and_then(|handover| handover.get("operation_id"))
        .and_then(Value::as_str)
        .unwrap_or(&terms.operation_id)
        .into();
    payload["clientRuntimeBuildId"] = terms.target_runtime_build_id.clone().into();
    payload["handover"] = handover.cloned().unwrap_or(Value::Null);
    HostOutcome {
        exit_code: HOST_EXIT_OK,
        payload,
    }
}
/// No answer is not a verdict: the socket says whether the target serves it now.
async fn reconcile_lost_reply(
    socket: &str,
    agent_dir: &std::path::Path,
    terms: &IdleHandoverTerms,
    before: &HostProtocolInfo,
) -> HostOutcome {
    let (now, build) = probe(socket).await;
    if build.as_deref() == Some(terms.target_runtime_build_id.as_str()) {
        return answered(
            socket,
            agent_dir,
            "handover_completed",
            now.as_ref(),
            terms,
            None,
        )
        .await;
    }
    let mut payload = identity_payload(
        socket,
        serving_pid(socket, agent_dir),
        now.as_ref().or(Some(before)),
    );
    payload["action"] = "error".into();
    payload["reason"] = "handover_reply_lost".into();
    payload["operationId"] = terms.operation_id.clone().into();
    HostOutcome {
        exit_code: HOST_EXIT_ERROR,
        payload,
    }
}
/// `client_runtime_build_id`: the runtime build id of the process that launches the successor, `None` when unreadable.
pub async fn idle_handover_outcome(
    socket: &str,
    agent_dir: &std::path::Path,
    options: &crate::host_handoff::HandoffOptions,
    terms: &IdleHandoverTerms,
    client_runtime_build_id: Option<&str>,
) -> std::io::Result<HostOutcome> {
    let body = |reason: &str, extra: Vec<(&str, Value)>| {
        let mut body = serde_json::Map::from_iter([
            ("reason".to_owned(), reason.into()),
            ("socket".to_owned(), socket.into()),
            ("operationId".to_owned(), terms.operation_id.clone().into()),
        ]);
        for (key, value) in extra {
            body.insert(key.to_owned(), value);
        }
        body
    };
    if cfg!(windows) {
        return Ok(refusal(
            false,
            None,
            body("upgrade_unsupported", Vec::new()),
        ));
    }
    let (before, before_build) = probe(socket).await;
    let refuse = |reason: &str, extra: Vec<(&str, Value)>| {
        refusal(false, before.as_ref(), body(reason, extra))
    };
    if client_runtime_build_id != Some(terms.target_runtime_build_id.as_str()) {
        return Ok(refuse(
            "target_build_mismatch",
            vec![
                (
                    "detail",
                    format!(
                        "this CLI launches {}",
                        client_runtime_build_id.unwrap_or("a runtime it cannot read")
                    )
                    .into(),
                ),
                (
                    "clientRuntimeBuildId",
                    client_runtime_build_id.map_or(Value::Null, Into::into),
                ),
            ],
        ));
    }
    let Some(host) = &before else {
        return Ok(refuse("no_host", Vec::new()));
    };
    if before_build.as_deref() == Some(terms.target_runtime_build_id.as_str()) {
        return Ok(answered(
            socket,
            agent_dir,
            "handover_completed",
            before.as_ref(),
            terms,
            None,
        )
        .await);
    }
    if host.instance_id.as_deref() != Some(terms.if_instance_id.as_str())
        || host.generation != Some(terms.if_generation as f64)
    {
        return Ok(refuse(
            "stale_generation",
            vec![(
                "detail",
                format!(
                    "the socket is served by {} generation {}",
                    host.instance_id.as_deref().unwrap_or("an unnamed host"),
                    host.generation
                        .map_or_else(|| "?".to_owned(), |generation| generation.to_string())
                )
                .into(),
            )],
        ));
    }
    if !host
        .capabilities
        .iter()
        .any(|capability| capability == RUNTIME_IDENTITY_HANDOVER_CAPABILITY)
    {
        return Ok(refuse("handover_unsupported", Vec::new()));
    }
    let reply = crate::host_probe::request_on_socket(
        socket,
        &begin_handover_command(options, terms)?,
        BEGIN_HANDOVER_TIMEOUT_MS,
    )
    .await;
    let Some(reply) = reply.filter(Value::is_object) else {
        return Ok(reconcile_lost_reply(socket, agent_dir, terms, host).await);
    };
    if let Some(reason) = reply.get("refused").and_then(Value::as_str) {
        return Ok(refuse(
            reason,
            reply
                .get("detail")
                .and_then(Value::as_str)
                .map(|detail| ("detail", detail.into()))
                .into_iter()
                .collect(),
        ));
    }
    match reply.get("state").and_then(Value::as_str) {
        Some("handover_blocked") => Ok(refuse(
            "handover_blocked",
            vec![
                (
                    "detail",
                    reply.get("reason").cloned().unwrap_or(Value::Null),
                ),
                ("handover", reply),
            ],
        )),
        Some("handover_completed") => {
            let (now, _) = probe(socket).await;
            Ok(answered(
                socket,
                agent_dir,
                "handover_completed",
                now.as_ref(),
                terms,
                Some(&reply),
            )
            .await)
        }
        _ => Ok(answered(
            socket,
            agent_dir,
            "handover_pending",
            before.as_ref(),
            terms,
            Some(&reply),
        )
        .await),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn options() -> crate::host_handoff::HandoffOptions {
        crate::host_handoff::HandoffOptions {
            host_args: vec![],
            policy: None,
            env: Default::default(),
            launch_profile_id: String::new(),
            readiness_ms: 1,
        }
    }
    fn terms() -> IdleHandoverTerms {
        IdleHandoverTerms {
            operation_id: "op".into(),
            if_instance_id: "i".into(),
            if_generation: 1,
            target_runtime_build_id: "new".into(),
        }
    }
    #[tokio::test]
    async fn a_client_that_cannot_prove_its_runtime_is_refused_before_any_connection() {
        let temp = tempfile::tempdir().unwrap();
        let outcome =
            idle_handover_outcome("/absent.sock", temp.path(), &options(), &terms(), None)
                .await
                .unwrap();
        assert_eq!(outcome.exit_code, crate::host_runner::HOST_EXIT_REFUSED);
        assert_eq!(outcome.payload["reason"], "target_build_mismatch");
        assert_eq!(outcome.payload["operationId"], "op");
        assert_eq!(
            outcome.payload["detail"],
            "this CLI launches a runtime it cannot read"
        );
    }
    #[tokio::test]
    async fn no_host_is_refused_once_the_client_runtime_matches() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("absent.sock");
        let outcome = idle_handover_outcome(
            socket.to_str().unwrap_or_default(),
            temp.path(),
            &options(),
            &terms(),
            Some("new"),
        )
        .await
        .unwrap();
        assert_eq!(
            (outcome.exit_code, outcome.payload["reason"].clone()),
            (crate::host_runner::HOST_EXIT_REFUSED, json!("no_host"))
        );
    }
    #[test]
    fn the_begin_handover_wire_names_the_terms_and_filters_the_environment() {
        let mut options = options();
        options.env.insert("PI_SESSION_ID".into(), Some("x".into()));
        options.env.insert("OMO_PROBE".into(), Some("1".into()));
        options.policy = Some(crate::host_lifecycle::HostLifecyclePolicy {
            cold_start: "transient".into(),
            idle_exit_ms: 5.,
        });
        let command = begin_handover_command(&options, &terms()).unwrap();
        assert_eq!(
            (
                command["type"].clone(),
                command["if_generation"].clone(),
                command["target_runtime_build_id"].clone()
            ),
            (json!("begin_handover"), json!(1), json!("new"))
        );
        assert_eq!(command["env"]["OMO_PROBE"], "1");
        assert!(command["env"].get("PI_SESSION_ID").is_none());
        assert_eq!(command["policy"]["coldStart"], "transient");
    }
}
