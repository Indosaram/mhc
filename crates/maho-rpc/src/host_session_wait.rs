//! `senpi host session wait` (senpi `rpc/host-session-wait.ts`, senpi#3073): settle on real host events,
//! never on elapsed time, and report a lost transport instead of waiting for the deadline.
use crate::host_runner::{HOST_EXIT_ERROR, HOST_EXIT_OK, HostOutcome};
use crate::rpc_client::{RpcClient, RpcClientError};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitUntil {
    Idle,
    Done,
}
impl WaitUntil {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Done => "done",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostSessionWait {
    pub until: WaitUntil,
    pub timeout_ms: u64,
}

/// What a forwarded event means to the wait.
enum Signal {
    Terminal(&'static str),
    Settled,
}

fn classify(event: &Value, until: WaitUntil) -> Option<Signal> {
    match event["type"].as_str()? {
        "session_closed" => Some(Signal::Terminal("session_closed")),
        "session_parked" => Some(Signal::Terminal("session_parked")),
        "agent_settled" if event["reason"] == "session_closed" => {
            Some(Signal::Terminal("session_closed"))
        }
        "agent_idle" => Some(Signal::Settled),
        "agent_settled" if until == WaitUntil::Done => Some(Signal::Settled),
        _ => None,
    }
}

fn is_drained(state: &Value) -> bool {
    let empty = |key: &str| state[key].as_array().is_none_or(Vec::is_empty);
    state["isStreaming"] != true
        && empty("steering")
        && empty("followUp")
        && empty("ordered")
        && state["pendingMessageCount"].as_u64().unwrap_or(0) == 0
}

/// Resolves only when the transport is gone: `wait_for_idle` answers `Err` exactly on transport loss and
/// `Ok` on each `agent_settled`, so looping on it is an event-driven loss signal, not a poll.
async fn transport_lost(client: &RpcClient) -> RpcClientError {
    loop {
        match client.wait_for_idle(Duration::MAX).await {
            Ok(()) => continue,
            Err(error) if error.is_transport_gone() => return error,
            Err(_) => continue,
        }
    }
}

/// Wait for `session_id` to reach `request.until`. `Err` carries a transport or command failure for the
/// caller's `map_error`.
pub async fn wait_for_host_session(
    client: &RpcClient,
    session_id: Option<&str>,
    request: HostSessionWait,
) -> Result<HostOutcome, RpcClientError> {
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(request.timeout_ms);
    let (events, mut received) = tokio::sync::mpsc::unbounded_channel::<Value>();
    // Subscribe BEFORE the snapshot, including when settlement arrives with that reply.
    let _subscription = client.on_event(std::sync::Arc::new(move |event: &Value| {
        let _waiter_gone = events.send(event.clone());
    }));
    let finish = |outcome: &str, state: Option<&Value>| HostOutcome {
        exit_code: HOST_EXIT_OK,
        payload: json!({
            "action": "wait",
            "sessionId": session_id,
            "until": request.until.as_str(),
            "outcome": outcome,
            "waited_ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "state": {
                "isStreaming": state.and_then(|state| state["isStreaming"].as_bool()).unwrap_or(false),
                "pendingMessageCount": state.and_then(|state| state["pendingMessageCount"].as_u64()).unwrap_or(0),
                "lastAbortSource": state.map_or(Value::Null, |state| state["lastAbortSource"].clone()),
            },
        }),
    };
    let waiting = wait_events(client, request, &mut received, &finish);
    match tokio::time::timeout_at(deadline, waiting).await {
        Ok(result) => result,
        Err(_) => Ok(HostOutcome {
            exit_code: HOST_EXIT_ERROR,
            payload: json!({
                "action": "error",
                "reason": "wait_timeout",
                "sessionId": session_id,
                "until": request.until.as_str(),
                "timeoutMs": request.timeout_ms,
            }),
        }),
    }
}

async fn wait_events(
    client: &RpcClient,
    request: HostSessionWait,
    received: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
    finish: &impl Fn(&str, Option<&Value>) -> HostOutcome,
) -> Result<HostOutcome, RpcClientError> {
    let mut observed = false;
    let mut snapshot = std::pin::pin!(client.get_state());
    let mut snapshot_pending = true;
    let lost = transport_lost(client);
    let mut lost = std::pin::pin!(lost);
    loop {
        tokio::select! {
            // Events queued ahead of the snapshot reply are handled first, as the TS listener runs
            // synchronously before the reply's promise resolves.
            biased;
            event = received.recv() => {
                // The sender lives in this wait's own subscription; a closed channel means the
                // client dropped its listeners with the transport.
                let Some(event) = event else {
                    return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "RPC transport is gone").into());
                };
                match classify(&event, request.until) {
                    None => {}
                    Some(Signal::Terminal(kind)) => return Ok(finish(kind, None)),
                    Some(Signal::Settled) => {
                        observed = true;
                        let state = client.get_state().await?;
                        if request.until == WaitUntil::Done {
                            return Ok(finish("done", Some(&state)));
                        }
                        if is_drained(&state) {
                            return Ok(finish("idle", Some(&state)));
                        }
                    }
                }
            }
            state = &mut snapshot, if snapshot_pending => {
                snapshot_pending = false;
                let state = state?;
                // A parked steer keeps `idle` waiting: it only drains with the next prompt.
                let settled = match request.until {
                    WaitUntil::Done => state["isStreaming"] != true,
                    WaitUntil::Idle => is_drained(&state),
                };
                if !observed && settled {
                    return Ok(finish("already_idle", Some(&state)));
                }
            }
            error = &mut lost => return Err(error),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::rpc_client::RpcClientOptions;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    /// A scripted host: `script(call, reply)` answers the `call`-th `get_state` with the lines it returns
    /// (events and the response, in wire order). Returns how many state reads the host served.
    fn wire_rig(
        script: impl Fn(usize, Value) -> Vec<Value> + Send + 'static,
    ) -> (RpcClient, tokio::task::JoinHandle<usize>) {
        let (client, host) = UnixStream::pair().expect("create UnixStream pair");
        let client = RpcClient::from_stream(client, RpcClientOptions::default());
        let host = tokio::spawn(async move {
            let (read, mut write) = host.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut reads = 0;
            while let Ok(Some(line)) = lines.next_line().await {
                let command: Value = serde_json::from_str(&line).expect("parse command JSON");
                if command["type"] != "get_state" {
                    continue;
                }
                reads += 1;
                let reply = json!({"type":"response","id":command["id"],"command":"get_state","success":true});
                let frames = script(reads, reply);
                if frames.is_empty() {
                    return reads;
                }
                for frame in frames {
                    let mut text = frame.to_string();
                    text.push('\n');
                    if write.write_all(text.as_bytes()).await.is_err() {
                        return reads;
                    }
                }
            }
            reads
        });
        (client, host)
    }

    fn with_data(mut reply: Value, data: Value) -> Value {
        reply["data"] = data;
        reply
    }

    fn idle() -> Value {
        json!({"isStreaming":false,"steering":[],"followUp":[],"ordered":[],"pendingMessageCount":0})
    }

    const DONE: HostSessionWait = HostSessionWait {
        until: WaitUntil::Done,
        timeout_ms: 5_000,
    };

    #[tokio::test]
    async fn does_not_miss_settlement_delivered_before_the_initial_state_reply() {
        let (client, host) = wire_rig(|call, reply| {
            let mut frames = Vec::new();
            if call == 1 {
                frames.push(json!({"type":"agent_settled"}));
            }
            frames.push(with_data(reply, idle()));
            frames
        });
        let outcome = wait_for_host_session(&client, Some("rpc-1"), DONE)
            .await
            .unwrap();
        assert_eq!(outcome.exit_code, HOST_EXIT_OK);
        assert_eq!(outcome.payload["outcome"], "done");
        client.stop().await;
        assert_eq!(host.await.unwrap(), 2);
    }

    #[tokio::test]
    async fn checks_queues_once_per_idle_event_and_ignores_low_level_run_and_turn_endings() {
        let (client, host) = wire_rig(|call, reply| {
            let mut frames = vec![with_data(
                reply,
                json!({
                    "isStreaming": call == 1,
                    "steering": if call == 2 { json!(["queued"]) } else { json!([]) },
                    "followUp": [],
                    "ordered": [],
                    "pendingMessageCount": u64::from(call == 2),
                }),
            )];
            if call == 1 {
                frames.push(json!({"type":"turn_end"}));
                frames.push(json!({"type":"agent_end"}));
                frames.push(json!({"type":"agent_settled"}));
            }
            if call < 3 {
                frames.push(json!({"type":"agent_idle"}));
            }
            frames
        });
        let outcome = wait_for_host_session(
            &client,
            Some("rpc-1"),
            HostSessionWait {
                until: WaitUntil::Idle,
                timeout_ms: 5_000,
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome.payload["outcome"], "idle");
        client.stop().await;
        assert_eq!(host.await.unwrap(), 3);
    }

    #[tokio::test]
    async fn reports_terminal_events_without_reading_a_dead_session() {
        for (event, expected) in [
            (
                json!({"type":"session_closed","reason":"host_shutdown"}),
                "session_closed",
            ),
            (
                json!({"type":"session_parked","reason":"host_shutdown"}),
                "session_parked",
            ),
            (
                json!({"type":"agent_settled","reason":"session_closed"}),
                "session_closed",
            ),
        ] {
            // The first reply never comes: only the terminal event can end the wait.
            let (client, _host) = wire_rig(move |_, _| vec![event.clone()]);
            let outcome = wait_for_host_session(&client, Some("rpc-1"), DONE)
                .await
                .unwrap();
            assert_eq!(outcome.exit_code, HOST_EXIT_OK);
            assert_eq!(outcome.payload["outcome"], expected);
            client.stop().await;
        }
    }

    #[tokio::test]
    async fn reports_a_lost_transport_instead_of_waiting_for_its_deadline() {
        let (client, _host) = wire_rig(|_, _| Vec::new());
        let error = wait_for_host_session(
            &client,
            Some("rpc-1"),
            HostSessionWait {
                until: WaitUntil::Done,
                timeout_ms: 60_000,
            },
        )
        .await;
        assert!(matches!(error, Err(ref error) if error.is_transport_gone()));
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_a_held_turn() {
        let (client, _host) = wire_rig(|_, reply| {
            vec![with_data(
                reply,
                json!({"isStreaming":true,"steering":[],"followUp":[],"ordered":[],"pendingMessageCount":0}),
            )]
        });
        let outcome = wait_for_host_session(
            &client,
            Some("rpc-1"),
            HostSessionWait {
                until: WaitUntil::Done,
                timeout_ms: 10,
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome.exit_code, HOST_EXIT_ERROR);
        assert_eq!(outcome.payload["reason"], "wait_timeout");
        assert_eq!(outcome.payload["timeoutMs"], 10);
        client.stop().await;
    }
}
