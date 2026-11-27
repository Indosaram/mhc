//! Port of senpi `packages/coding-agent/src/modes/rpc/attach-takeover.ts`.
//!
//! `attach_session { takeover: true, holder }` on a machine-owned host (R21; S37).

use crate::attach_ledger::AttachLedger;
use async_trait::async_trait;
use serde_json::{Value, json};

#[async_trait]
pub trait TakeoverHost {
    fn is_machine_owned(&self) -> bool;
    fn ledger(&self) -> Option<&AttachLedger>;
    fn after_commit_refusal(
        &self,
        id: Option<&Value>,
        session_id: &str,
        owner: &str,
    ) -> Option<Value>;
    fn attach(&mut self, session_id: &str, owner: &str) -> Result<(), (String, Value)>;
    fn take_other_holds(&mut self, session_id: &str, owner: &str) -> Vec<(String, usize)>;
    fn has_connection(&self, connection_id: &str) -> bool;
    async fn enqueue_control(&mut self, event: Value, connection_id: &str);
    async fn release_disconnect_attachment(&mut self, session_id: &str);
}

fn error_response(id: Option<&Value>, code: &str, data: Value) -> Value {
    json!({
        "id": id,
        "type": "response",
        "command": "attach_session",
        "success": false,
        "error": code,
        "errorCode": code,
        "errorData": data,
    })
}

pub async fn take_over(
    host: &mut dyn TakeoverHost,
    id: Option<&Value>,
    session_id: &str,
    durable_session_id: &str,
    holder: Option<&str>,
    owner: &str,
) -> Value {
    let Some(ledger) = host.ledger() else {
        return error_response(
            id,
            "takeover_unsupported",
            json!({"reason": "only a headless engine with a takeover ledger takes sessions over"}),
        );
    };
    if !host.is_machine_owned() {
        return error_response(
            id,
            "takeover_unsupported",
            json!({"reason": "only a headless engine with a takeover ledger takes sessions over"}),
        );
    }
    let Some(holder) = holder.filter(|h| !h.is_empty()) else {
        return error_response(id, "takeover_holder_required", json!({}));
    };
    let record = match ledger.take_over(durable_session_id, holder).await {
        Ok(r) => r,
        Err(reason) => {
            return error_response(id, "takeover_failed", json!({"reason": reason}));
        }
    };
    if let Some(refusal) = host.after_commit_refusal(id, session_id, owner) {
        return refusal;
    }
    if let Err((code, data)) = host.attach(session_id, owner) {
        return error_response(id, &code, data);
    }
    let taken = host.take_other_holds(session_id, owner);
    let released = json!({
        "type": "session_released",
        "sessionId": session_id,
        "reason": "taken_over",
    });
    for (connection, holds) in &taken {
        if host.has_connection(connection) {
            host.enqueue_control(released.clone(), connection).await;
        }
        for _ in 0..*holds {
            host.release_disconnect_attachment(session_id).await;
        }
    }
    json!({
        "id": id,
        "type": "response",
        "command": "attach_session",
        "success": true,
        "data": {
            "sessionId": session_id,
            "attached": true,
            "holder": record.holder,
            "generation": record.generation,
            "released": taken.len(),
        },
    })
}
