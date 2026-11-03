//! Wire answers for `subscribe_sessions`, `unsubscribe_sessions` and `set_session_context` (senpi
//! `session-status-commands.ts`).
//!
//! Every refusal follows the error rule: `error` keeps the stable `<code>: <reason>` string existing clients switch
//! on, and `errorData { reason, action }` says in plain words what went wrong and what the caller should send instead.
use crate::rpc_types::{
    RPC_ERROR_INVALID_SESSION_CONTEXT, RPC_ERROR_INVALID_SUBSCRIPTION,
    RPC_ERROR_SESSION_NOT_ATTACHED, RPC_ERROR_SUBSCRIPTION_LIMIT, RPC_ERROR_UNKNOWN_SUBSCRIPTION,
    RPC_ERROR_UNSUPPORTED_TRANSPORT,
};
use crate::session_context_patch::{SessionContextOverlay, apply_context_patch};
use crate::session_status_stream::{
    MAX_SUBSCRIPTIONS_PER_CONNECTION, SessionStatusDelivery, SessionStatusSource,
    SessionStatusStream,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// A refusal's plain-words detail.
pub struct RefusalDetail<'a> {
    pub reason: &'a str,
    pub action: &'a str,
}

fn with_id(mut response: Value, id: Option<&str>) -> Value {
    if let Some(id) = id {
        response["id"] = id.into();
    }
    response
}

pub fn refusal(id: Option<&str>, command: &str, code: &str, detail: RefusalDetail<'_>) -> Value {
    with_id(
        json!({
            "type": "response",
            "command": command,
            "success": false,
            "error": format!("{code}: {}", detail.reason),
            "errorCode": code,
            "errorData": {"reason": detail.reason, "action": detail.action},
        }),
        id,
    )
}

/// The answer to one `subscribe_sessions`.
pub struct SubscribeAnswer {
    pub answer: Value,
    /// Minted subscription id to activate once the answer is on the connection's FIFO; absent on a refusal.
    pub activate: Option<String>,
}

pub fn answer_subscribe_sessions(
    command: &Value,
    connection_id: Option<&str>,
    stream: &mut SessionStatusStream,
    source: &dyn SessionStatusSource,
    delivery: &dyn SessionStatusDelivery,
) -> SubscribeAnswer {
    let id = command["id"].as_str();
    let refused = |code: &str, detail: RefusalDetail<'_>| SubscribeAnswer {
        answer: refusal(id, "subscribe_sessions", code, detail),
        activate: None,
    };
    let Some(connection_id) = connection_id else {
        return refused(
            RPC_ERROR_UNSUPPORTED_TRANSPORT,
            RefusalDetail {
                reason: "stdio has no per-connection identity to deliver a subscription to",
                action: "connect to the host socket (senpi --mode rpc --listen) and subscribe there",
            },
        );
    };
    if stream.connection_size(connection_id) >= MAX_SUBSCRIPTIONS_PER_CONNECTION {
        return refused(
            RPC_ERROR_SUBSCRIPTION_LIMIT,
            RefusalDetail {
                reason: &format!(
                    "this connection already holds {MAX_SUBSCRIPTIONS_PER_CONNECTION} subscriptions"
                ),
                action: "unsubscribe_sessions the ones you no longer read, then subscribe again",
            },
        );
    }
    let options = match crate::session_status_subscriptions::parse_subscription(command) {
        Ok(options) => options,
        Err(parsed) => {
            return refused(
                RPC_ERROR_INVALID_SUBSCRIPTION,
                RefusalDetail {
                    reason: &parsed.reason,
                    action: &parsed.action,
                },
            );
        }
    };
    let subscribed = stream.subscribe(source, delivery, connection_id, options);
    // The response must reach the connection's FIFO before the subscription goes live: a delta published meanwhile
    // queues behind it, and `seq 0` (the snapshot) is always the first record the client reads for this id.
    let env = std::env::vars().collect();
    SubscribeAnswer {
        answer: with_id(
            json!({
                "type": "response",
                "command": "subscribe_sessions",
                "success": true,
                "data": {
                    "subscriptionId": subscribed.subscription_id,
                    "snapshot": subscribed.snapshot,
                    "seq": subscribed.seq,
                    "hostGeneration": crate::protocol_identity::host_generation(&env).to_string(),
                    "hostInstanceId": crate::protocol_identity::host_instance_id(),
                },
            }),
            id,
        ),
        activate: Some(subscribed.subscription_id),
    }
}

pub fn answer_unsubscribe_sessions(
    command: &Value,
    connection_id: Option<&str>,
    stream: &mut SessionStatusStream,
) -> Value {
    let id = command["id"].as_str();
    let Some(connection_id) = connection_id else {
        return refusal(
            id,
            "unsubscribe_sessions",
            RPC_ERROR_UNSUPPORTED_TRANSPORT,
            RefusalDetail {
                reason: "stdio has no per-connection identity holding any subscription",
                action: "connect to the host socket (senpi --mode rpc --listen) and manage subscriptions there",
            },
        );
    };
    if command["subscriptionId"]
        .as_str()
        .is_some_and(|subscription| stream.unsubscribe(connection_id, subscription))
    {
        return with_id(
            json!({"type":"response","command":"unsubscribe_sessions","success":true}),
            id,
        );
    }
    refusal(
        id,
        "unsubscribe_sessions",
        RPC_ERROR_UNKNOWN_SUBSCRIPTION,
        RefusalDetail {
            reason: "this connection holds no subscription with that id",
            action: "send the subscriptionId your subscribe_sessions answer returned, on the same connection",
        },
    )
}

pub fn not_attached_refusal(id: Option<&str>) -> Value {
    refusal(
        id,
        "set_session_context",
        RPC_ERROR_SESSION_NOT_ATTACHED,
        RefusalDetail {
            reason: "this connection is not attached to that session",
            action: "attach first with open_session { sessionPath }, then patch its context",
        },
    )
}

/// Applies a `set_session_context` patch to the entry's overlay and answers it. On success the caller publishes the
/// `context` change (`publish(sessionId, "context", "client", true)`), which upstream does inside this function; the
/// Rust stream sits behind the host's own locks.
pub fn answer_set_session_context(
    id: Option<&str>,
    context: &BTreeMap<String, String>,
    overlay: &mut Option<SessionContextOverlay>,
    patch: &Value,
) -> Result<Value, Value> {
    let applied = apply_context_patch(overlay.as_ref(), patch).map_err(|refused| {
        refusal(
            id,
            "set_session_context",
            RPC_ERROR_INVALID_SESSION_CONTEXT,
            RefusalDetail {
                reason: &refused.reason,
                action: &refused.action,
            },
        )
    })?;
    *overlay = Some(applied);
    Ok(with_id(
        json!({
            "type": "response",
            "command": "set_session_context",
            "success": true,
            "data": {"context": crate::session_context_patch::published_session_context(context, overlay.as_ref())},
        }),
        id,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_registry::RpcSessionState;
    use crate::session_status::{SessionStatusEntryView, SessionView};
    use std::sync::Mutex;

    struct OneSession;
    impl SessionStatusSource for OneSession {
        fn session_ids(&self) -> Vec<String> {
            vec!["s1".into()]
        }
        fn view(&self, _session_id: &str) -> Option<SessionView> {
            Some(SessionView {
                entry: SessionStatusEntryView {
                    state: RpcSessionState::Open,
                    kind: "interactive",
                    cwd: "/w".into(),
                    attachments: 0,
                    durable_session_id: None,
                    session_path: None,
                    name: None,
                    context: serde_json::Map::new(),
                    open_context: None,
                },
                runtime: Some(Default::default()),
            })
        }
    }
    #[derive(Default)]
    struct Sent(Mutex<Vec<String>>);
    impl SessionStatusDelivery for Sent {
        fn enqueue(&self, _connection_id: &str, line: &str) -> bool {
            self.0.lock().unwrap().push(line.to_owned());
            true
        }
        fn pending_questions(&self, _session_id: &str) -> usize {
            0
        }
    }

    /// rpc-session-status.test.ts "stdio transports refuse subscribe/unsubscribe with a code that names the
    /// transport".
    #[test]
    fn stdio_refuses_subscribe_and_unsubscribe_naming_the_transport() {
        let mut stream = SessionStatusStream::default();
        let subscribe = answer_subscribe_sessions(
            &json!({"id":"s","type":"subscribe_sessions"}),
            None,
            &mut stream,
            &OneSession,
            &Sent::default(),
        );
        assert!(subscribe.activate.is_none());
        assert_eq!(subscribe.answer["errorCode"], "unsupported_transport");
        assert_eq!(
            subscribe.answer["error"],
            "unsupported_transport: stdio has no per-connection identity to deliver a subscription to"
        );
        let unsubscribe = answer_unsubscribe_sessions(
            &json!({"id":"u","type":"unsubscribe_sessions","subscriptionId":"sub-1"}),
            None,
            &mut stream,
        );
        assert_eq!(unsubscribe["errorCode"], "unsupported_transport");
        assert_eq!(unsubscribe["id"], "u");
    }

    /// rpc-session-status.test.ts "refuses a 65th subscription on one connection with subscription_limit and
    /// reason/action"; an invalid selector is refused before anything is minted; an unknown id is refused.
    #[test]
    fn the_65th_subscription_invalid_selectors_and_unknown_ids_are_refused() {
        let mut stream = SessionStatusStream::default();
        let sent = Sent::default();
        let subscribe = |stream: &mut SessionStatusStream, request: Value| {
            answer_subscribe_sessions(&request, Some("c1"), stream, &OneSession, &sent)
        };
        let first = subscribe(&mut stream, json!({"id":"1","type":"subscribe_sessions"}));
        assert_eq!(first.answer["data"]["seq"], 0);
        assert_eq!(first.answer["data"]["snapshot"][0]["sessionId"], "s1");
        assert!(first.answer["data"]["hostInstanceId"].is_string());
        for _ in 1..MAX_SUBSCRIPTIONS_PER_CONNECTION {
            assert!(
                subscribe(&mut stream, json!({"type":"subscribe_sessions"}))
                    .activate
                    .is_some()
            );
        }
        let limited = subscribe(&mut stream, json!({"id":"65","type":"subscribe_sessions"}));
        assert_eq!(limited.answer["errorCode"], "subscription_limit");
        assert_eq!(
            limited.answer["errorData"]["action"],
            "unsubscribe_sessions the ones you no longer read, then subscribe again"
        );
        let mut other = SessionStatusStream::default();
        let invalid = answer_subscribe_sessions(
            &json!({"type":"subscribe_sessions","dialogs":"yes"}),
            Some("c1"),
            &mut other,
            &OneSession,
            &sent,
        );
        assert_eq!(invalid.answer["errorCode"], "invalid_subscription");
        assert_eq!(other.connection_size("c1"), 0);
        let unknown = answer_unsubscribe_sessions(
            &json!({"type":"unsubscribe_sessions","subscriptionId":"sub-999"}),
            Some("c1"),
            &mut stream,
        );
        assert_eq!(unknown["errorCode"], "unknown_subscription");
        let removed = answer_unsubscribe_sessions(
            &json!({"type":"unsubscribe_sessions","subscriptionId":first.activate.unwrap()}),
            Some("c1"),
            &mut stream,
        );
        assert_eq!(removed["success"], true);
    }

    /// rpc-session-status.test.ts "patches context, republishes it to include_context subscribers and refuses
    /// reserved keys with reason and action".
    #[test]
    fn patches_context_and_refuses_reserved_keys_and_unattached_callers() {
        let context = BTreeMap::from([("parent_session_id".to_owned(), "p".to_owned())]);
        let mut overlay = None;
        let answer = answer_set_session_context(
            Some("c"),
            &context,
            &mut overlay,
            &json!({"task_status":"running"}),
        )
        .unwrap();
        assert_eq!(answer["data"]["context"]["task_status"], "running");
        assert_eq!(answer["data"]["context"]["parent_session_id"], "p");
        let refused = answer_set_session_context(
            Some("r"),
            &context,
            &mut overlay,
            &json!({"host_socket":"x"}),
        )
        .unwrap_err();
        assert_eq!(refused["errorCode"], "invalid_session_context");
        assert!(refused["errorData"]["action"].is_string());
        assert_eq!(overlay.unwrap()["task_status"], "running");
        let unattached = not_attached_refusal(Some("n"));
        assert_eq!(unattached["errorCode"], "session_not_attached");
        assert_eq!(
            unattached["error"],
            "session_not_attached: this connection is not attached to that session"
        );
    }
}
