//! Port of senpi `packages/coding-agent/src/modes/rpc/session-router-open-result.ts`.
//!
//! Shared ownership transition, publication, and reply builder for both freshly built
//! `open_session` and claimed speculative `claim_session` / `move_session import`.

use serde_json::{Map, Value, json};

#[derive(Debug, Clone)]
pub struct OpenReplyAs {
    pub command: String,
    pub id: Option<Value>,
    pub data: Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct OpenResultSummary {
    pub session_id: String,
    pub durable_session_id: Option<String>,
    pub session_path: Option<String>,
    pub kind: String,
    pub cwd: String,
    pub context: Map<String, Value>,
    pub attached: bool,
}

pub trait OpenResultHost {
    fn ensure_connection_registered(&mut self, owner: &str);
    fn attach_connection_to_session(&mut self, owner: &str, session_id: &str, compact: bool);
    fn record_owned_session(&mut self, owner: &str, session_id: &str);
    fn record_attach_order(&mut self, session_id: &str, owner: &str);
    fn publish_status(&mut self, session_id: &str, event: &str, actor: &str);
    fn publish_session_opened(&mut self, payload: Value);
    fn enqueue_response(&mut self, session_id: &str, response: Value);
}

pub fn own_open(
    host: &mut dyn OpenResultHost,
    session_id: &str,
    owner: Option<&str>,
    compact: bool,
) {
    let Some(owner) = owner else {
        return;
    };
    host.ensure_connection_registered(owner);
    host.attach_connection_to_session(owner, session_id, compact);
    host.record_owned_session(owner, session_id);
    host.record_attach_order(session_id, owner);
}

pub fn publish_open(host: &mut dyn OpenResultHost, summary: &OpenResultSummary) {
    if summary.attached {
        host.publish_status(&summary.session_id, "attach", "client");
        return;
    }
    let mut event = json!({
        "type": "session_opened",
        "sessionId": summary.session_id,
        "kind": summary.kind,
        "cwd": summary.cwd,
        "context": Value::Object(summary.context.clone()),
    });
    if let Some(ref did) = summary.durable_session_id {
        event["durableSessionId"] = json!(did);
    }
    if let Some(ref path) = summary.session_path {
        event["sessionPath"] = json!(path);
    }
    host.publish_session_opened(event);
    host.publish_status(&summary.session_id, "open", "client");
}

pub fn build_open_reply_payload(
    session_id: &str,
    state: Value,
    attached: bool,
    beside: Option<&Map<String, Value>>,
    presence: Option<Value>,
) -> Value {
    let mut data = Map::new();
    if let Some(extra) = beside {
        for (k, v) in extra {
            data.insert(k.clone(), v.clone());
        }
    }
    data.insert("sessionId".to_owned(), json!(session_id));
    data.insert("state".to_owned(), state);
    if attached {
        data.insert("attached".to_owned(), json!(true));
    }
    if let Some(p) = presence {
        data.insert("presence".to_owned(), p);
    }
    Value::Object(data)
}

#[allow(clippy::too_many_arguments)]
pub fn reply_open(
    host: &mut dyn OpenResultHost,
    session_id: &str,
    command: &str,
    id: Option<&Value>,
    state: Value,
    attached: bool,
    beside: Option<&Map<String, Value>>,
    presence: Option<Value>,
) {
    let data = build_open_reply_payload(session_id, state, attached, beside, presence);
    host.enqueue_response(
        session_id,
        json!({
            "id": id,
            "type": "response",
            "command": command,
            "success": true,
            "data": data,
        }),
    );
}
