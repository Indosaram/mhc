//! Short-lived, attach-before-act clients for `senpi host session` (senpi `rpc/host-session-runner.ts`,
//! senpi#3073). Each request opens one connection, resolves its session reference, acts, then asks the host
//! to drop the connection's attachments before closing. Callers tell a refusal, a caller error and a broken
//! transport apart by exit code and `reason`, never by scraping prose.
use crate::host_endpoints::{EndpointKind, endpoint_kind_of_socket};
use crate::host_ensure::{EnsureHostError, EnsureHostOptions, EnsuredHostInfo, HostUpgradePolicy};
use crate::host_launch_spec::ResolvedHostLaunchSpec;
use crate::host_runner::{
    HOST_EXIT_ERROR, HOST_EXIT_OK, HOST_EXIT_REFUSED, HOST_EXIT_USAGE, HostOutcome, HostTarget,
    refusal,
};
use crate::host_session_detach::{DETACH_ACK_TIMEOUT_MS, detach_before_close};
use crate::host_session_ref::{ResolvedSessionRef, SessionRef, resolve_session_ref};
use crate::host_session_wait::{HostSessionWait, wait_for_host_session};
use crate::rpc_client::{
    ListSessionsOptions, RpcClient, RpcClientError, RpcClientOptions, is_transport_gone_error,
    prompt_disposition_name,
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

/// A model named by provider and id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostSessionModel {
    pub provider: String,
    pub id: String,
}

/// senpi `HostSessionOpen`: what an `open` asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostSessionOpen {
    pub cwd: String,
    pub model: Option<HostSessionModel>,
    pub name: Option<String>,
    pub prompt: Option<String>,
}

/// What `unload` does with live task children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnloadChildren {
    Refuse,
    Detach,
}
impl UnloadChildren {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Refuse => "refuse",
            Self::Detach => "detach",
        }
    }
}

/// One `host session` action (senpi `HostSessionRequest` minus the target).
#[derive(Debug)]
pub enum HostSessionAction {
    Open {
        spec: ResolvedHostLaunchSpec,
        open: HostSessionOpen,
    },
    Close {
        reference: SessionRef,
    },
    Unload {
        reference: SessionRef,
        interrupt: bool,
        force: bool,
        clear_queue: bool,
        children: UnloadChildren,
    },
    Model {
        reference: SessionRef,
        model: HostSessionModel,
    },
    Prompt {
        reference: SessionRef,
        text: String,
    },
    Steer {
        reference: SessionRef,
        text: String,
    },
    Abort {
        reference: SessionRef,
    },
    Read {
        reference: SessionRef,
        tail: Option<usize>,
        since: Option<String>,
        messages: bool,
    },
    State {
        reference: SessionRef,
    },
    List,
    Wait {
        reference: SessionRef,
        wait: HostSessionWait,
    },
}
impl HostSessionAction {
    fn reference(&self) -> Option<&str> {
        match self {
            Self::Open { .. } | Self::List => None,
            Self::Close { reference }
            | Self::Unload { reference, .. }
            | Self::Model { reference, .. }
            | Self::Prompt { reference, .. }
            | Self::Steer { reference, .. }
            | Self::Abort { reference }
            | Self::Read { reference, .. }
            | Self::State { reference }
            | Self::Wait { reference, .. } => Some(reference.as_str()),
        }
    }
}

pub struct HostSessionRequest {
    pub target: HostTarget,
    pub action: HostSessionAction,
}

/// senpi `STOP_WAIT_BUDGET_MS` (`host-ensure-stop.ts`): how long an `open` keeps retrying a stopping
/// generation. Sized from the same `SENPI_RPC_CHILD_STALLED_STOP_MAX_MS` the supervisor bounds itself by.
fn stop_wait_budget_ms() -> u64 {
    use crate::host_stop_intent::{CHILD_KILL_EXIT_TIMEOUT_MS, CHILD_STOP_TIMEOUT_MS};
    const DEFAULT_CHILD_STALLED_STOP_MAX_MS: u64 = 60_000;
    const SIGKILL_GRACE_MS: u64 = 2_000;
    let stalled = std::env::var("SENPI_RPC_CHILD_STALLED_STOP_MAX_MS")
        .ok()
        .and_then(|value| crate::host_lifecycle::parse_idle_exit_ms(Some(value.as_str())))
        .map_or(DEFAULT_CHILD_STALLED_STOP_MAX_MS, |ms| ms as u64);
    let default_stop = CHILD_STOP_TIMEOUT_MS.max(stalled) + CHILD_KILL_EXIT_TIMEOUT_MS + 5_000;
    let after_stall = CHILD_KILL_EXIT_TIMEOUT_MS + 5_000;
    default_stop.max(stalled + after_stall) + SIGKILL_GRACE_MS
}

/// Perform one `host session` request. Never fails: every error becomes an outcome.
pub async fn run_host_session_request(request: HostSessionRequest) -> HostOutcome {
    let retry = matches!(request.action, HostSessionAction::Open { .. });
    let budget = Duration::from_millis(stop_wait_budget_ms());
    match retry_while_host_stopping(retry, budget, || run_host_session_request_once(&request)).await
    {
        Ok(outcome) => outcome,
        Err(error) => map_error(&error, &request.target.socket),
    }
}

/// senpi `ensureHostLocked` polls a stopping generation every 50 ms (`delay(Math.min(50, remaining))`).
const HOST_STOPPING_RETRY_MS: u64 = 50;

/// senpi `runHostSessionRequest`: an `open` that meets a stopping generation tries again until the
/// stop budget runs out; every other error, and any error from another action, is returned at once.
/// Rust `ensure_host` refuses a stopping host at once instead of waiting for it like senpi, so each
/// retry waits on the tokio clock first. Without that wait, reconnects would spin.
async fn retry_while_host_stopping<F, Fut>(
    retry: bool,
    budget: Duration,
    mut once: F,
) -> Result<HostOutcome, RpcClientError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<HostOutcome, RpcClientError>>,
{
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        match once().await {
            Err(error)
                if retry
                    && error.error_code() == Some("host_stopping")
                    && tokio::time::Instant::now() < deadline =>
            {
                let remaining = deadline - tokio::time::Instant::now();
                tokio::time::sleep(remaining.min(Duration::from_millis(HOST_STOPPING_RETRY_MS)))
                    .await;
            }
            result => return result,
        }
    }
}

fn body(entries: Vec<(&'static str, Value)>) -> Map<String, Value> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

fn process_env() -> BTreeMap<String, Option<String>> {
    std::env::vars()
        .map(|(name, value)| (name, Some(value)))
        .collect()
}

fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    }
}

async fn ensure_for_open(
    target: &HostTarget,
    spec: &ResolvedHostLaunchSpec,
) -> Result<EnsuredHostInfo, HostOutcome> {
    let socket = &target.socket;
    let env = crate::host_daemon_env::daemon_env_overrides(&process_env(), &spec.env, platform());
    let ensured = crate::host_ensure::ensure_host(EnsureHostOptions {
        socket: socket.clone(),
        agent_dir: Some(target.agent_dir.clone()),
        policy: Some(spec.policy.clone()),
        host_args: spec.host_args.clone(),
        env: env.into_iter().collect::<HashMap<_, _>>(),
        upgrade: HostUpgradePolicy::Never,
        ..Default::default()
    })
    .await;
    let ensured = match ensured {
        Ok(ensured) => ensured,
        Err(EnsureHostError::Refused(reason)) => {
            return Err(refusal(
                false,
                None,
                body(vec![
                    ("reason", reason.into()),
                    ("socket", socket.as_str().into()),
                    ("detail", format!("host ensure refused: {reason}").into()),
                ]),
            ));
        }
        Err(EnsureHostError::Io(error)) => return Err(host_error(&error.to_string(), socket)),
    };
    if !ensured.reused {
        let keys = crate::host_daemon_env::daemon_env_keys(&process_env(), &spec.env, platform());
        let paths = crate::host_daemon_paths::create_host_daemon_paths(socket, &target.agent_dir);
        if let Err(error) = crate::host_daemon_env::write_daemon_env_keys(&paths, &keys) {
            return Err(host_error(&error.to_string(), socket));
        }
    }
    Ok(ensured)
}

/// A host that stopped answering or a lost transport cannot confirm a detach; the socket close releases.
fn connection_unusable(error: &RpcClientError) -> bool {
    match error {
        RpcClientError::Transport(error) => {
            error.kind() == std::io::ErrorKind::TimedOut || is_transport_gone_error(error)
        }
        RpcClientError::Command(_) => false,
    }
}

async fn run_host_session_request_once(
    request: &HostSessionRequest,
) -> Result<HostOutcome, RpcClientError> {
    let socket = &request.target.socket;
    let agent_dir = &request.target.agent_dir;
    if endpoint_kind_of_socket(socket, agent_dir) == EndpointKind::Tui {
        return Ok(refusal(
            false,
            None,
            body(vec![
                ("reason", "unsupported_endpoint_kind".into()),
                ("socket", socket.as_str().into()),
                ("endpoint_kind", "tui".into()),
            ]),
        ));
    }
    let mut ensured = None;
    if let HostSessionAction::Open { spec, .. } = &request.action {
        match ensure_for_open(&request.target, spec).await {
            Ok(info) => ensured = Some(info),
            Err(outcome) => return Ok(outcome),
        }
    }
    let client = RpcClient::new(RpcClientOptions {
        socket_path: Some(socket.into()),
        agent_dir: Some(agent_dir.clone()),
        ..Default::default()
    });
    match client.start().await {
        Ok(()) => {}
        Err(RpcClientError::Transport(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(refusal(
                false,
                None,
                body(vec![
                    ("reason", "host_unavailable".into()),
                    ("socket", socket.as_str().into()),
                    ("detail", error.to_string().into()),
                ]),
            ));
        }
        Err(error) => return Err(error),
    }
    let result = act(&client, request, ensured.as_ref()).await;
    if !result.as_ref().err().is_some_and(connection_unusable) {
        detach_before_close(
            client.detach_connection(),
            |line| eprintln!("senpi host session: {line}"),
            DETACH_ACK_TIMEOUT_MS,
        )
        .await;
    }
    client.stop().await;
    result
}

fn ok(payload: Value) -> HostOutcome {
    HostOutcome {
        exit_code: HOST_EXIT_OK,
        payload,
    }
}

async fn act(
    client: &RpcClient,
    request: &HostSessionRequest,
    ensured: Option<&EnsuredHostInfo>,
) -> Result<HostOutcome, RpcClientError> {
    let socket = request.target.socket.as_str();
    let mut session_id = None;
    let mut durable_session_id = None;
    if let Some(reference) = request.action.reference() {
        let rows = client.list_sessions(ListSessionsOptions::default()).await?;
        let reopen = match resolve_session_ref(&rows, reference, &request.target.agent_dir) {
            ResolvedSessionRef::Unknown => {
                return Ok(refusal(
                    false,
                    None,
                    body(vec![
                        ("reason", "unknown_session".into()),
                        ("ref", reference.into()),
                        ("socket", socket.into()),
                    ]),
                ));
            }
            ResolvedSessionRef::Ambiguous(candidates) => {
                return Ok(refusal(
                    false,
                    None,
                    body(vec![
                        ("reason", "ambiguous_session".into()),
                        ("ref", reference.into()),
                        ("candidates", candidates.into()),
                        ("socket", socket.into()),
                    ]),
                ));
            }
            // A parked session resumes from its transcript and stays retained past this short-lived client.
            ResolvedSessionRef::Parked {
                session_path,
                cwd,
                durable_session_id: durable,
            } => {
                durable_session_id = Some(durable);
                json!({"sessionPath": session_path, "cwd": cwd, "retain_on_disconnect": true, "attended": false})
            }
            ResolvedSessionRef::Live(row) => {
                durable_session_id = row["durableSessionId"].as_str().map(str::to_owned);
                let mut reopen = json!({});
                for key in ["sessionPath", "cwd"] {
                    if let Some(value) = row.get(key).filter(|value| !value.is_null()) {
                        reopen[key] = value.clone();
                    }
                }
                reopen
            }
        };
        let opened = client.open_session(reopen).await?;
        session_id = opened["sessionId"].as_str().map(str::to_owned);
    }
    let sid = session_id.as_deref();
    Ok(match &request.action {
        HostSessionAction::Open { open, .. } => {
            return open_host_session(client, open, socket, ensured).await;
        }
        HostSessionAction::Prompt { text, .. } => {
            let disposition = client.prompt(text, json!({})).await?;
            ok(
                json!({"action": "prompt", "sessionId": sid, "disposition": prompt_disposition_name(disposition)}),
            )
        }
        HostSessionAction::Steer { text, .. } => {
            // An idle steer is parked until the next prompt; it does not start a run.
            let streaming = client.get_state().await?["isStreaming"] == true;
            let disposition = client.steer_with_disposition(text, None, None).await?;
            ok(
                json!({"action": "steer", "sessionId": sid, "disposition": disposition.as_str(), "streaming": streaming}),
            )
        }
        HostSessionAction::Abort { .. } => {
            let aborted = client.get_state().await?["isStreaming"] == true;
            client.abort_strict().await?;
            ok(
                json!({"action": "abort", "sessionId": sid, "acknowledged": true, "aborted": aborted}),
            )
        }
        HostSessionAction::Model { model, .. } => {
            // set_model also persists the GLOBAL default. Smaller contexts may defer the switch.
            client.set_model(&model.provider, &model.id).await?;
            let state = client.get_state().await?;
            let current = match state.get("model").filter(|model| model.is_object()) {
                Some(model) => json!({"provider": model["provider"], "id": model["id"]}),
                None => Value::Null,
            };
            ok(json!({
                "action": "model",
                "sessionId": sid,
                "requested": {"provider": model.provider, "id": model.id},
                "model": current,
                "pendingModelSwitch": state.get("pendingModelSwitch").cloned().unwrap_or(Value::Null),
            }))
        }
        HostSessionAction::List => {
            let sessions = client
                .list_sessions(ListSessionsOptions {
                    observe: Some(true),
                    include_workers: None,
                })
                .await?;
            ok(json!({"action": "list", "socket": socket, "sessions": sessions}))
        }
        HostSessionAction::State { .. } => {
            let state = client.get_state().await?;
            ok(
                json!({"action": "state", "sessionId": sid, "durableSessionId": durable_session_id, "state": state}),
            )
        }
        HostSessionAction::Read {
            tail,
            since,
            messages,
            ..
        } => {
            if *messages {
                let all = array(client.get_messages().await?, "messages");
                let messages = tail_of(&all, *tail);
                ok(
                    json!({"action": "read", "sessionId": sid, "count": messages.len(), "messages": messages}),
                )
            } else {
                let data = client.get_entries(since.as_deref()).await?;
                let entries = array(data.clone(), "entries");
                let slice = tail_of(&entries, *tail);
                let next_since = slice
                    .last()
                    .and_then(|entry| entry.get("id").cloned())
                    .or_else(|| since.clone().map(Value::from))
                    .unwrap_or(Value::Null);
                let mut payload = json!({
                    "action": "read",
                    "sessionId": sid,
                    "entries": slice,
                    "count": slice.len(),
                    "total": entries.len(),
                    "leafId": data.get("leafId").cloned().unwrap_or(Value::Null),
                    "nextSince": next_since,
                });
                if let Some(since) = since {
                    payload["since"] = since.as_str().into();
                }
                ok(payload)
            }
        }
        HostSessionAction::Wait { wait, .. } => {
            return wait_for_host_session(client, sid, *wait).await;
        }
        HostSessionAction::Unload {
            interrupt,
            force,
            clear_queue,
            children,
            ..
        } => {
            let answer = client
                .unload_session(
                    sid,
                    json!({"interrupt": interrupt, "force": force, "clearQueue": clear_queue, "children": children.as_str()}),
                )
                .await?;
            let exit_code = if answer["outcome"] == "unloaded" {
                HOST_EXIT_OK
            } else {
                HOST_EXIT_REFUSED
            };
            let mut payload = json!({"action": "unload", "sessionId": sid});
            if let Some(answer) = answer.as_object() {
                for (key, value) in answer {
                    payload[key] = value.clone();
                }
            }
            HostOutcome { exit_code, payload }
        }
        HostSessionAction::Close { .. } => {
            let reason = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
            let seen = reason.clone();
            let _subscription = client.on_event(std::sync::Arc::new(move |event: &Value| {
                if event["type"] == "session_closed" {
                    *seen
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        event["reason"].as_str().map(str::to_owned);
                }
            }));
            client.close_session(sid).await?;
            // close_session tolerates disconnect; this checked read must still succeed.
            let remaining = client
                .list_sessions(ListSessionsOptions::default())
                .await?
                .into_iter()
                .find(|row| row["sessionId"].as_str() == sid);
            let reason = reason
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            ok(match remaining {
                Some(row) => {
                    json!({"action": "close", "sessionId": sid, "closed": false, "attachments": row["attachments"]})
                }
                None => {
                    json!({"action": "close", "sessionId": sid, "closed": true, "reason": reason.unwrap_or_else(|| "client_close".into())})
                }
            })
        }
    })
}

fn array(mut data: Value, key: &str) -> Vec<Value> {
    match data.get_mut(key).map(Value::take) {
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    }
}

/// senpi `entries.slice(-tail)`: `-0` is `0`, so a zero tail keeps every item.
fn tail_of(items: &[Value], tail: Option<usize>) -> Vec<Value> {
    let start = tail
        .filter(|tail| *tail > 0)
        .map_or(0, |tail| items.len().saturating_sub(tail));
    items[start..].to_vec()
}

/// senpi `openHostSession` (`host-session-open.ts`): open a retained session; a later step that fails
/// closes it again, or names it when that close fails.
pub async fn open_host_session(
    client: &RpcClient,
    request: &HostSessionOpen,
    socket: &str,
    ensured: Option<&EnsuredHostInfo>,
) -> Result<HostOutcome, RpcClientError> {
    let mut options = json!({"cwd": request.cwd, "retain_on_disconnect": true});
    if let Some(model) = &request.model {
        options["provider"] = model.provider.as_str().into();
        options["modelId"] = model.id.as_str().into();
    }
    let opened = client.open_session(options).await?;
    let state = &opened["state"];
    let session_id = opened["sessionId"].as_str().unwrap_or_default().to_owned();
    let identity = [
        ("sessionId", Value::from(session_id.as_str())),
        ("sessionPath", state["sessionFile"].clone()),
        ("durableSessionId", state["sessionId"].clone()),
    ];
    match open_steps(client, request).await {
        Ok(disposition) => {
            let mut payload = json!({"action": "open", "socket": socket});
            if let Some(ensured) = ensured {
                payload["pid"] = ensured.pid.into();
                payload["reused"] = ensured.reused.into();
            }
            for (key, value) in identity {
                payload[key] = value;
            }
            payload["cwd"] = state["cwd"].clone();
            payload["model"] = match state.get("model").filter(|model| model.is_object()) {
                Some(model) => json!({"provider": model["provider"], "id": model["id"]}),
                None => Value::Null,
            };
            payload["attached"] = opened["attached"].as_bool().unwrap_or(false).into();
            if let Some(disposition) = disposition {
                payload["prompt"] = json!({"disposition": prompt_disposition_name(disposition)});
            }
            Ok(ok(payload))
        }
        Err(error) => {
            let mut outcome = map_error(&error, socket);
            let closed = close_opened(client, &session_id).await;
            for (key, value) in identity {
                outcome.payload[key] = value;
            }
            outcome.payload["closed"] = closed.into();
            Ok(outcome)
        }
    }
}

async fn open_steps(
    client: &RpcClient,
    request: &HostSessionOpen,
) -> Result<Option<crate::rpc_client::PromptDisposition>, RpcClientError> {
    if let Some(name) = &request.name {
        client.set_session_name(name).await?;
    }
    match &request.prompt {
        Some(prompt) => Ok(Some(client.prompt(prompt, json!({})).await?)),
        None => Ok(None),
    }
}

async fn close_opened(client: &RpcClient, session_id: &str) -> bool {
    if client.close_session(Some(session_id)).await.is_err() {
        // `closed: false` plus the ids in the payload is how the caller learns it must close it.
        return false;
    }
    client
        .list_sessions(ListSessionsOptions::default())
        .await
        .is_ok_and(|rows| rows.iter().all(|row| row["sessionId"] != session_id))
}

const USAGE_CODES: &[&str] = &[
    "missing_session_id",
    "invalid_path",
    "invalid_session_context",
    "invalid_session_kind",
    "invalid_launch_profile",
    "invalid_session_id",
    "invalid_release_reason",
    "empty",
    "unknown_command",
];
const FAILURE_CODES: &[&str] = &["open_failed", "warm_failed", "release_failed"];

/// The session router answers with its bare stable code as the whole `error` string.
fn bare_code(detail: &str) -> Option<&str> {
    let mut bytes = detail.bytes();
    (bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'))
    .then_some(detail)
}

fn host_error(detail: &str, socket: &str) -> HostOutcome {
    HostOutcome {
        exit_code: HOST_EXIT_ERROR,
        payload: json!({"action": "error", "reason": "host_error", "detail": detail, "socket": socket}),
    }
}

/// senpi `mapError` (`host-session-errors.ts`): a host refusal becomes a usage error, a failure or a
/// refusal by its stable code; a lost transport is `transport_gone`; anything else is `host_error`.
pub fn map_error(error: &RpcClientError, socket: &str) -> HostOutcome {
    let detail = error.to_string();
    match error {
        RpcClientError::Command(command) => {
            let Some(code) = command
                .error_code
                .clone()
                .or_else(|| bare_code(&detail).map(str::to_owned))
            else {
                return host_error(&detail, socket);
            };
            let exit_code = if USAGE_CODES.contains(&code.as_str()) {
                HOST_EXIT_USAGE
            } else if FAILURE_CODES.contains(&code.as_str()) {
                HOST_EXIT_ERROR
            } else {
                HOST_EXIT_REFUSED
            };
            let reason = match code.as_str() {
                "streaming" => "busy".to_owned(),
                "not_found" => "unknown_cursor".to_owned(),
                other => other.to_owned(),
            };
            let action = if exit_code == HOST_EXIT_REFUSED {
                "refuse"
            } else {
                "error"
            };
            let mut payload = json!({
                "action": action,
                "reason": reason,
                "detail": detail,
                "socket": socket,
            });
            if let Some(data) = &command.error_data {
                payload["data"] = data.clone();
            }
            HostOutcome { exit_code, payload }
        }
        RpcClientError::Transport(io) if is_transport_gone_error(io) => HostOutcome {
            exit_code: HOST_EXIT_ERROR,
            payload: json!({"action": "error", "reason": "transport_gone", "detail": detail, "socket": socket}),
        },
        RpcClientError::Transport(_) => host_error(&detail, socket),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::host_session_wait::WaitUntil;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    struct FakeSession {
        id: String,
        path: String,
        cwd: String,
        durable: String,
        name: Option<String>,
        attachments: u64,
        streaming: bool,
        steering: Vec<String>,
        model: &'static str,
    }

    /// A scripted multi-session host on a real socket path: every runner request is a new connection.
    #[derive(Default)]
    struct Fake {
        sessions: Vec<FakeSession>,
        entries: Vec<Value>,
        messages: Vec<Value>,
        seen: Vec<Value>,
        refuse_abort: bool,
        refuse_open: bool,
        unload_answer: Value,
        next: u64,
    }

    impl Fake {
        fn seed(&mut self, id: &str, name: Option<&str>, attachments: u64) {
            self.sessions.push(FakeSession {
                id: id.into(),
                path: format!("/sessions/{id}.jsonl"),
                cwd: "/work".into(),
                durable: format!("durable-{id}"),
                name: name.map(str::to_owned),
                attachments,
                streaming: false,
                steering: Vec::new(),
                model: "mock-claude-rpc",
            });
        }
        fn row(session: &FakeSession) -> Value {
            json!({"sessionId": session.id, "durableSessionId": session.durable, "sessionPath": session.path,
                "cwd": session.cwd, "name": session.name, "status": "open", "attachments": session.attachments})
        }
        fn state(session: &FakeSession) -> Value {
            json!({"isStreaming": session.streaming, "sessionId": session.durable, "sessionFile": session.path,
                "cwd": session.cwd, "model": {"provider": "anthropic", "id": session.model},
                "pendingModelSwitch": null, "steering": session.steering, "followUp": [], "ordered": [],
                "pendingMessageCount": session.steering.len()})
        }
        fn index(&self, command: &Value) -> Option<usize> {
            let id = command["sessionId"].as_str()?;
            self.sessions.iter().position(|session| session.id == id)
        }

        fn handle(&mut self, command: &Value, attached: &mut Vec<String>) -> Vec<Value> {
            self.seen.push(command.clone());
            let ok = |data: Value| json!({"type":"response","id":command["id"],"command":command["type"],"success":true,"data":data});
            let refuse = |message: &str, code: Option<&str>| json!({"type":"response","id":command["id"],"command":command["type"],"success":false,"error":message,"errorCode":code});
            let session = self.index(command);
            match command["type"].as_str().unwrap_or_default() {
                "get_protocol_info" => vec![ok(json!({"capabilities": ["detach_connection"]}))],
                "detach_connection" => {
                    for id in attached.drain(..) {
                        if let Some(session) = self.sessions.iter_mut().find(|s| s.id == id) {
                            session.attachments -= 1;
                        }
                    }
                    vec![ok(json!({"deferred": []}))]
                }
                "list_sessions" => {
                    let rows: Vec<Value> = self.sessions.iter().map(Self::row).collect();
                    vec![ok(json!({ "sessions": rows }))]
                }
                "open_session" => {
                    if self.refuse_open {
                        return vec![refuse(
                            "Session path is not a session file",
                            Some("invalid_path"),
                        )];
                    }
                    let index = match command["sessionPath"].as_str() {
                        Some(path) => self.sessions.iter().position(|s| s.path == path),
                        None => {
                            self.next += 1;
                            self.seed(&format!("rpc-{}", self.next), None, 0);
                            Some(self.sessions.len() - 1)
                        }
                    };
                    let Some(index) = index else {
                        return vec![refuse("No session at that path", Some("invalid_path"))];
                    };
                    let session = &mut self.sessions[index];
                    session.attachments += 1;
                    attached.push(session.id.clone());
                    vec![ok(
                        json!({"sessionId": session.id, "state": Self::state(session), "attached": false}),
                    )]
                }
                "close_session" => {
                    let Some(index) = session else {
                        return vec![refuse("missing_session_id", Some("missing_session_id"))];
                    };
                    let id = self.sessions[index].id.clone();
                    attached.retain(|held| *held != id);
                    self.sessions[index].attachments -= 1;
                    if self.sessions[index].attachments > 0 {
                        return vec![ok(json!({}))];
                    }
                    self.sessions.remove(index);
                    vec![
                        json!({"type":"session_closed","sessionId":id,"reason":"client_close"}),
                        ok(json!({})),
                    ]
                }
                "set_session_name" => {
                    if let Some(index) = session {
                        self.sessions[index].name = command["name"].as_str().map(str::to_owned);
                    }
                    vec![ok(json!({}))]
                }
                "get_state" => vec![ok(Self::state(&self.sessions[session.unwrap()]))],
                "get_messages" => vec![ok(json!({"messages": self.messages}))],
                "get_entries" => {
                    let start = match command["since"].as_str() {
                        None => 0,
                        Some(since) => match self.entries.iter().position(|e| e["id"] == since) {
                            Some(position) => position + 1,
                            None => return vec![refuse("not_found", Some("not_found"))],
                        },
                    };
                    let entries = self.entries[start..].to_vec();
                    let leaf = self.entries.last().map(|entry| entry["id"].clone());
                    vec![ok(json!({ "entries": entries, "leafId": leaf }))]
                }
                "prompt" => {
                    let session = &mut self.sessions[session.unwrap()];
                    if command["message"]
                        .as_str()
                        .is_some_and(|text| text.len() > 1_000)
                    {
                        return vec![refuse("prompt is too large", None)];
                    }
                    if session.streaming {
                        return vec![refuse("streaming", Some("streaming"))];
                    }
                    session.streaming = true;
                    vec![ok(json!({"disposition": "started"}))]
                }
                "steer" => {
                    let session = &mut self.sessions[session.unwrap()];
                    session
                        .steering
                        .push(command["message"].as_str().unwrap().into());
                    vec![ok(json!({"disposition": "queued"}))]
                }
                "abort" => {
                    if self.refuse_abort {
                        return vec![refuse("abort_refused", Some("abort_refused"))];
                    }
                    self.sessions[session.unwrap()].streaming = false;
                    vec![ok(json!({}))]
                }
                "set_model" => {
                    let model = match command["modelId"].as_str() {
                        Some("mock-claude-rpc") => "mock-claude-rpc",
                        Some("mock-claude-rpc-2") => "mock-claude-rpc-2",
                        _ => return vec![refuse("model_not_found", Some("model_not_found"))],
                    };
                    self.sessions[session.unwrap()].model = model;
                    vec![ok(json!({}))]
                }
                "unload_session" => vec![ok(self.unload_answer.clone())],
                _ => vec![refuse("unknown_command", Some("unknown_command"))],
            }
        }
    }

    struct Rig {
        _dir: tempfile::TempDir,
        target: HostTarget,
        fake: Arc<Mutex<Fake>>,
    }

    impl Rig {
        fn new(seed: impl FnOnce(&mut Fake)) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("host.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let mut fake = Fake::default();
            seed(&mut fake);
            let fake = Arc::new(Mutex::new(fake));
            let shared = fake.clone();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let fake = shared.clone();
                    tokio::spawn(async move {
                        let (read, mut write) = stream.into_split();
                        let mut lines = BufReader::new(read).lines();
                        let mut attached = Vec::new();
                        while let Ok(Some(line)) = lines.next_line().await {
                            let command: Value = serde_json::from_str(&line).unwrap();
                            let frames = fake.lock().unwrap().handle(&command, &mut attached);
                            for frame in frames {
                                if write
                                    .write_all(format!("{frame}\n").as_bytes())
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                    });
                }
            });
            Self {
                target: HostTarget {
                    socket: socket.to_string_lossy().into_owned(),
                    agent_dir: dir.path().to_path_buf(),
                },
                _dir: dir,
                fake,
            }
        }
        fn target(&self) -> HostTarget {
            HostTarget {
                socket: self.target.socket.clone(),
                agent_dir: self.target.agent_dir.clone(),
            }
        }
        async fn run(&self, action: HostSessionAction) -> HostOutcome {
            run_host_session_request(HostSessionRequest {
                target: self.target(),
                action,
            })
            .await
        }
        fn with<R>(&self, f: impl FnOnce(&mut Fake) -> R) -> R {
            f(&mut self.fake.lock().unwrap())
        }
        fn seen(&self, kind: &str) -> Vec<Value> {
            self.with(|fake| {
                fake.seen
                    .iter()
                    .filter(|command| command["type"] == kind)
                    .cloned()
                    .collect()
            })
        }
    }

    fn reference(id: &str) -> SessionRef {
        id.into()
    }

    #[tokio::test]
    async fn closes_idle_sessions_and_refuses_a_second_close() {
        let rig = Rig::new(|fake| fake.seed("rpc-1", None, 0));
        let closed = rig
            .run(HostSessionAction::Close {
                reference: reference("rpc-1"),
            })
            .await;
        assert_eq!(closed.exit_code, 0);
        assert_eq!(closed.payload["closed"], true);
        assert_eq!(closed.payload["reason"], "client_close");
        assert!(rig.with(|fake| fake.sessions.is_empty()));
        let again = rig
            .run(HostSessionAction::Close {
                reference: reference("rpc-1"),
            })
            .await;
        assert_eq!(again.exit_code, 3);
        assert_eq!(again.payload["reason"], "unknown_session");
    }

    #[tokio::test]
    async fn reports_another_holder_instead_of_force_closing_its_session() {
        let rig = Rig::new(|fake| fake.seed("rpc-1", None, 1));
        let outcome = rig
            .run(HostSessionAction::Close {
                reference: reference("rpc-1"),
            })
            .await;
        assert_eq!(outcome.exit_code, 0);
        assert_eq!(outcome.payload["closed"], false);
        assert_eq!(outcome.payload["attachments"], 1);
        assert_eq!(rig.with(|fake| fake.sessions[0].attachments), 1);
    }

    #[tokio::test]
    async fn reads_append_order_cursors_tails_messages_state_and_observe_only_listings() {
        let rig = Rig::new(|fake| {
            fake.seed("rpc-1", None, 0);
            fake.entries = (1..=4).map(|n| json!({"id": format!("e{n}")})).collect();
            fake.messages = vec![
                json!({"role":"user","content":"unique-313 read-me"}),
                json!({"role":"assistant"}),
            ];
        });
        let read = |tail: Option<usize>, since: Option<&str>, messages: bool| {
            rig.run(HostSessionAction::Read {
                reference: reference("rpc-1"),
                tail,
                since: since.map(str::to_owned),
                messages,
            })
        };
        let entries = rig.with(|fake| fake.entries.clone());
        let all = read(None, None, false).await;
        assert_eq!(all.exit_code, 0);
        assert_eq!(all.payload["entries"], json!(entries));
        assert_eq!(all.payload["total"], 4);
        assert_eq!(all.payload["count"], 4);
        assert_eq!(all.payload["leafId"], "e4");
        let tail = read(Some(2), None, false).await;
        assert_eq!(tail.payload["entries"], json!(entries[2..]));
        assert_eq!(tail.payload["count"], 2);
        assert_eq!(tail.payload["nextSince"], "e4");
        assert_eq!(
            read(Some(5), None, false).await.payload["entries"],
            json!(entries)
        );
        // senpi `slice(-0)` is `slice(0)`: a zero tail returns everything.
        let zero = read(Some(0), None, false).await;
        assert_eq!(zero.payload["entries"], json!(entries));
        assert_eq!(zero.payload["count"], 4);
        assert_eq!(read(Some(0), None, true).await.payload["count"], 2);
        assert_eq!(
            read(None, Some("e2"), false).await.payload["entries"],
            json!(entries[2..])
        );
        let since_tail = read(Some(1), Some("e2"), false).await;
        assert_eq!(since_tail.payload["entries"], json!(entries[3..]));
        assert_eq!(since_tail.payload["since"], "e2");
        let at_end = read(None, Some("e4"), false).await;
        assert_eq!(at_end.payload["count"], 0);
        assert_eq!(at_end.payload["nextSince"], "e4");
        let messages = read(None, None, true).await;
        assert!(
            messages.payload["messages"]
                .to_string()
                .contains("unique-313 read-me")
        );
        assert_eq!(read(Some(1), None, true).await.payload["count"], 1);
        let unknown_cursor = read(None, Some("nope"), false).await;
        assert_eq!(unknown_cursor.exit_code, 3);
        assert_eq!(unknown_cursor.payload["reason"], "unknown_cursor");
        let state = rig
            .run(HostSessionAction::State {
                reference: reference("rpc-1"),
            })
            .await;
        assert_eq!(state.exit_code, 0);
        assert_eq!(state.payload["durableSessionId"], "durable-rpc-1");
        assert_eq!(state.payload["state"]["isStreaming"], false);
        let listed = rig.run(HostSessionAction::List).await;
        assert_eq!(listed.exit_code, 0);
        assert_eq!(listed.payload["sessions"][0]["sessionId"], "rpc-1");
        assert_eq!(listed.payload["sessions"][0]["attachments"], 0);
        // Only the listing a `list` action sends is marked as an observe-only read.
        let observed = rig.seen("list_sessions");
        assert_eq!(observed.last().unwrap()["observe"], true);
        assert!(
            observed[..observed.len() - 1]
                .iter()
                .all(|c| c.get("observe").is_none())
        );
        let unknown = rig
            .run(HostSessionAction::Read {
                reference: reference("unknown"),
                tail: None,
                since: None,
                messages: false,
            })
            .await;
        assert_eq!(unknown.exit_code, 3);
        assert_eq!(unknown.payload["reason"], "unknown_session");
    }

    #[tokio::test]
    async fn reports_busy_prompts_queued_steering_and_idle_steering_without_starting_a_turn() {
        let rig = Rig::new(|fake| fake.seed("rpc-1", None, 0));
        let prompt = |text: &str| {
            rig.run(HostSessionAction::Prompt {
                reference: reference("rpc-1"),
                text: text.into(),
            })
        };
        let steer = |text: &str| {
            rig.run(HostSessionAction::Steer {
                reference: reference("rpc-1"),
                text: text.into(),
            })
        };
        let started = prompt("unique-1").await;
        assert_eq!(started.exit_code, 0);
        assert_eq!(started.payload["disposition"], "started");
        let busy = prompt("busy").await;
        assert_eq!(busy.exit_code, 3);
        assert_eq!(busy.payload["reason"], "busy");
        let queued = steer("unique-2").await;
        assert_eq!(queued.exit_code, 0);
        assert_eq!(queued.payload["disposition"], "queued");
        assert_eq!(queued.payload["streaming"], true);
        rig.with(|fake| fake.sessions[0].streaming = false);
        let parked = steer("parked").await;
        assert_eq!(parked.payload["disposition"], "queued");
        assert_eq!(parked.payload["streaming"], false);
        assert!(!rig.with(|fake| fake.sessions[0].streaming));
        // A parked steer is not idle: `idle` keeps waiting while `done` sees no running turn.
        let idle = rig
            .run(HostSessionAction::Wait {
                reference: reference("rpc-1"),
                wait: HostSessionWait {
                    until: WaitUntil::Idle,
                    timeout_ms: 100,
                },
            })
            .await;
        assert_eq!(idle.exit_code, 1);
        assert_eq!(idle.payload["reason"], "wait_timeout");
        let done = rig
            .run(HostSessionAction::Wait {
                reference: reference("rpc-1"),
                wait: HostSessionWait {
                    until: WaitUntil::Done,
                    timeout_ms: 20_000,
                },
            })
            .await;
        assert_eq!(done.exit_code, 0);
        assert_eq!(done.payload["outcome"], "already_idle");
    }

    #[tokio::test]
    async fn acknowledges_aborts_changes_models_and_surfaces_a_refused_abort() {
        let rig = Rig::new(|fake| fake.seed("rpc-1", None, 0));
        let abort = || {
            rig.run(HostSessionAction::Abort {
                reference: reference("rpc-1"),
            })
        };
        let model = |id: &str| {
            rig.run(HostSessionAction::Model {
                reference: reference("rpc-1"),
                model: HostSessionModel {
                    provider: "anthropic".into(),
                    id: id.into(),
                },
            })
        };
        rig.with(|fake| fake.sessions[0].streaming = true);
        let switched = model("mock-claude-rpc-2").await;
        assert_eq!(switched.exit_code, 0);
        assert_eq!(switched.payload["model"]["id"], "mock-claude-rpc-2");
        assert_eq!(switched.payload["pendingModelSwitch"], Value::Null);
        let aborted = abort().await;
        assert_eq!(aborted.exit_code, 0);
        assert_eq!(aborted.payload["acknowledged"], true);
        assert_eq!(aborted.payload["aborted"], true);
        assert_eq!(abort().await.payload["aborted"], false);
        assert_eq!(
            model("mock-claude-rpc").await.payload["model"]["id"],
            "mock-claude-rpc"
        );
        let missing = model("nope").await;
        assert_eq!(missing.exit_code, 3);
        assert_eq!(missing.payload["reason"], "model_not_found");
        rig.with(|fake| fake.refuse_abort = true);
        let refused = abort().await;
        assert_eq!(refused.exit_code, 3);
        assert_eq!(refused.payload["reason"], "abort_refused");
    }

    #[tokio::test]
    async fn unload_reports_the_hosts_answer_and_sends_its_options() {
        let rig = Rig::new(|fake| {
            fake.seed("rpc-1", None, 0);
            fake.unload_answer =
                json!({"outcome":"refused","reason":"turn_active","action":"abort first"});
        });
        let unload = || {
            rig.run(HostSessionAction::Unload {
                reference: reference("rpc-1"),
                interrupt: true,
                force: false,
                clear_queue: true,
                children: UnloadChildren::Detach,
            })
        };
        let refused = unload().await;
        assert_eq!(refused.exit_code, 3);
        // senpi spreads the answer over the payload, so the host's `action` hint replaces "unload".
        assert_eq!(refused.payload["action"], "abort first");
        assert_eq!(refused.payload["reason"], "turn_active");
        let sent = rig.seen("unload_session");
        assert_eq!(sent[0]["sessionId"], "rpc-1");
        assert_eq!(sent[0]["interrupt"], true);
        assert_eq!(sent[0]["force"], false);
        assert_eq!(sent[0]["clearQueue"], true);
        assert_eq!(sent[0]["children"], "detach");
        rig.with(|fake| {
            fake.unload_answer = json!({"outcome":"unloaded","sessionPath":"/sessions/rpc-1.jsonl"})
        });
        let unloaded = unload().await;
        assert_eq!(unloaded.exit_code, 0);
        assert_eq!(unloaded.payload["sessionPath"], "/sessions/rpc-1.jsonl");
    }

    #[tokio::test]
    async fn detaches_its_connection_before_closing() {
        let rig = Rig::new(|fake| fake.seed("rpc-1", None, 0));
        rig.run(HostSessionAction::State {
            reference: reference("rpc-1"),
        })
        .await;
        assert_eq!(rig.seen("detach_connection").len(), 1);
        assert_eq!(rig.with(|fake| fake.sessions[0].attachments), 0);
    }

    #[tokio::test]
    async fn refuses_a_name_that_matches_more_than_one_session() {
        let rig = Rig::new(|fake| {
            fake.seed("rpc-1", Some("lane-1"), 0);
            fake.seed("rpc-2", Some("lane-1"), 0);
        });
        let outcome = rig
            .run(HostSessionAction::State {
                reference: reference("lane-1"),
            })
            .await;
        assert_eq!(outcome.exit_code, 3);
        assert_eq!(outcome.payload["reason"], "ambiguous_session");
        assert_eq!(outcome.payload["candidates"], json!(["rpc-1", "rpc-2"]));
    }

    #[tokio::test]
    async fn opens_a_retained_named_session_with_a_prompt() {
        let rig = Rig::new(|_| {});
        let client = RpcClient::connect(std::path::Path::new(&rig.target.socket))
            .await
            .unwrap();
        let open = HostSessionOpen {
            cwd: "/work".into(),
            name: Some("lane-1".into()),
            prompt: Some("hello".into()),
            ..Default::default()
        };
        let opened = open_host_session(&client, &open, &rig.target.socket, None)
            .await
            .unwrap();
        assert_eq!(opened.exit_code, 0);
        assert_eq!(opened.payload["sessionId"], "rpc-1");
        assert_eq!(opened.payload["durableSessionId"], "durable-rpc-1");
        assert_eq!(opened.payload["model"]["id"], "mock-claude-rpc");
        assert_eq!(opened.payload["attached"], false);
        assert_eq!(opened.payload["prompt"]["disposition"], "started");
        assert_eq!(rig.seen("open_session")[0]["retain_on_disconnect"], true);
        client.stop().await;
    }

    // #3073 review H2: a refusal after the session exists must not leave it retained and unaddressable.
    #[tokio::test]
    async fn closes_the_session_it_opened_when_a_later_open_step_fails_and_names_it() {
        let rig = Rig::new(|fake| fake.seed("rpc-0", None, 1));
        let client = RpcClient::connect(std::path::Path::new(&rig.target.socket))
            .await
            .unwrap();
        let open = HostSessionOpen {
            cwd: "/work".into(),
            prompt: Some("x".repeat(1_001)),
            ..Default::default()
        };
        let outcome = open_host_session(&client, &open, &rig.target.socket, None)
            .await
            .unwrap();
        assert_ne!(outcome.exit_code, 0);
        assert_eq!(outcome.payload["sessionId"], "rpc-1");
        assert_eq!(outcome.payload["closed"], true);
        let ids = rig.with(|fake| {
            fake.sessions
                .iter()
                .map(|s| s.id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(ids, vec!["rpc-0".to_owned()]);
        client.stop().await;
    }

    #[tokio::test]
    async fn refuses_an_unavailable_host_without_starting_one() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_host_session_request(HostSessionRequest {
            target: HostTarget {
                socket: dir
                    .path()
                    .join("absent.sock")
                    .to_string_lossy()
                    .into_owned(),
                agent_dir: dir.path().to_path_buf(),
            },
            action: HostSessionAction::List,
        })
        .await;
        assert_eq!(outcome.exit_code, 3);
        assert_eq!(outcome.payload["action"], "refuse");
        assert_eq!(outcome.payload["reason"], "host_unavailable");
    }

    #[tokio::test]
    async fn rejects_a_terminal_endpoint_before_connecting() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_host_session_request(HostSessionRequest {
            target: HostTarget {
                socket: dir
                    .path()
                    .join("t-0123456789abcdef.sock")
                    .to_string_lossy()
                    .into_owned(),
                agent_dir: dir.path().to_path_buf(),
            },
            action: HostSessionAction::List,
        })
        .await;
        assert_eq!(outcome.exit_code, 3);
        assert_eq!(outcome.payload["reason"], "unsupported_endpoint_kind");
    }

    // A refused open_session keeps the host's errorCode: a prose message still maps by code, not by text.
    #[tokio::test]
    async fn a_refused_open_session_exits_with_its_error_code_as_a_usage_error() {
        let rig = Rig::new(|fake| {
            fake.seed("rpc-1", None, 0);
            fake.refuse_open = true;
        });
        let outcome = rig
            .run(HostSessionAction::State {
                reference: reference("rpc-1"),
            })
            .await;
        assert_eq!(outcome.exit_code, 2);
        assert_eq!(outcome.payload["action"], "error");
        assert_eq!(outcome.payload["reason"], "invalid_path");
        assert_eq!(
            outcome.payload["detail"],
            "Session path is not a session file"
        );
        assert_eq!(rig.seen("open_session").len(), 1);
        assert_eq!(rig.with(|fake| fake.sessions[0].attachments), 0);
    }

    fn stopping() -> RpcClientError {
        RpcClientError::Command(crate::rpc_client::RpcCommandError {
            message: "host_stopping".into(),
            error_code: Some("host_stopping".into()),
            error_data: None,
        })
    }

    #[tokio::test(start_paused = true)]
    async fn an_open_retries_a_stopping_host_until_it_succeeds() {
        let attempts = std::cell::Cell::new(0);
        let result = retry_while_host_stopping(true, Duration::from_secs(60), || {
            attempts.set(attempts.get() + 1);
            let attempt = attempts.get();
            async move {
                if attempt < 3 {
                    Err(stopping())
                } else {
                    Ok(ok(json!({"action": "open"})))
                }
            }
        })
        .await;
        assert_eq!(result.unwrap().payload["action"], "open");
        assert_eq!(attempts.get(), 3);
    }

    // The paused tokio clock only advances through the retry sleeps, so elapsed time counts them exactly.
    #[tokio::test(start_paused = true)]
    async fn a_stopping_retry_waits_between_attempts_and_gives_up_at_the_budget() {
        let started = tokio::time::Instant::now();
        let attempts = std::cell::Cell::new(0);
        let result = retry_while_host_stopping(true, Duration::from_millis(120), || {
            attempts.set(attempts.get() + 1);
            async { Err(stopping()) }
        })
        .await;
        assert_eq!(result.unwrap_err().error_code(), Some("host_stopping"));
        // Waits of 50, 50 and the remaining 20 ms, then one last attempt at the deadline.
        assert_eq!(attempts.get(), 4);
        assert_eq!(started.elapsed(), Duration::from_millis(120));
    }

    #[tokio::test(start_paused = true)]
    async fn only_an_open_retries_a_stopping_host() {
        let attempts = std::cell::Cell::new(0);
        let result = retry_while_host_stopping(false, Duration::from_secs(60), || {
            attempts.set(attempts.get() + 1);
            async { Err(stopping()) }
        })
        .await;
        assert_eq!(result.unwrap_err().error_code(), Some("host_stopping"));
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn maps_refusal_codes_usage_codes_and_transport_loss() {
        let command = |message: &str, code: Option<&str>| {
            RpcClientError::Command(crate::rpc_client::RpcCommandError {
                message: message.into(),
                error_code: code.map(str::to_owned),
                error_data: None,
            })
        };
        let usage = map_error(&command("bad", Some("invalid_path")), "/s");
        assert_eq!(
            (usage.exit_code, usage.payload["action"].clone()),
            (2, json!("error"))
        );
        let bare = map_error(&command("host_stopping", None), "/s");
        assert_eq!(
            (bare.exit_code, bare.payload["reason"].clone()),
            (3, json!("host_stopping"))
        );
        let prose = map_error(&command("Something broke", None), "/s");
        assert_eq!(
            (prose.exit_code, prose.payload["reason"].clone()),
            (1, json!("host_error"))
        );
        let gone = map_error(
            &RpcClientError::Transport(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone")),
            "/s",
        );
        assert_eq!(gone.payload["reason"], "transport_gone");
    }
}
