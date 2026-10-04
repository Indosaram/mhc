use super::approval_types::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use tokio::sync::oneshot;

struct PendingApproval {
    thread_id: String,
    request: Value,
    allow_key: Option<String>,
    resolve: oneshot::Sender<ApprovalOutcome>,
}
pub struct ApprovalBridge {
    next_id: u64,
    pending: BTreeMap<u64, PendingApproval>,
    session_allows: BTreeSet<String>,
    send: SendToThreadSubscribers,
}
impl ApprovalBridge {
    pub fn new(send: SendToThreadSubscribers) -> Self { Self { next_id:0, pending:BTreeMap::new(), session_allows:BTreeSet::new(), send } }
    pub fn pending_count(&self) -> usize { self.pending.len() }
    pub fn request_approval(&mut self, thread_id: &str, kind: ApprovalKind, payload: &Value, now: u64) -> oneshot::Receiver<ApprovalOutcome> {
        let (resolve, receiver) = oneshot::channel();
        let allow_key = payload["toolName"].as_str().filter(|name| !name.is_empty()).zip(payload["command"].as_str()).map(|(name, command)| format!("{thread_id}\0{}\0{name}\0{command}", match kind { ApprovalKind::CommandExecution => "commandExecution", ApprovalKind::FileChange => "fileChange" }));
        if allow_key.as_ref().is_some_and(|key| self.session_allows.contains(key)) {
            let _delivery = resolve.send(ApprovalOutcome { allow:true, decision:ApprovalDecision::AcceptForSession, reason:None });
            return receiver;
        }
        let id = self.next_id; self.next_id += 1;
        let mut params = json!({"threadId":thread_id,"turnId":payload["turnId"].as_str().unwrap_or("turn-approval"),"itemId":payload["itemId"].as_str().unwrap_or(match kind { ApprovalKind::CommandExecution => "approval-command", ApprovalKind::FileChange => "approval-file-change" }),"startedAtMs":now,"reason":payload["reason"]});
        let method = match kind {
            ApprovalKind::FileChange => { params["grantRoot"] = payload["grantRoot"].clone(); "item/fileChange/requestApproval" },
            ApprovalKind::CommandExecution => {
                for key in ["approvalId", "environmentId", "command", "cwd"] { params[key] = payload[key].clone(); }
                params["availableDecisions"] = json!(APPROVAL_DECISIONS);
                "item/commandExecution/requestApproval"
            },
        };
        let request = json!({"id":id,"method":method,"params":params});
        self.pending.insert(id, PendingApproval { thread_id:thread_id.into(), request:request.clone(), allow_key, resolve });
        if (self.send)(thread_id, request) == 0 && let Some(pending) = self.pending.remove(&id) {
            let _delivery = pending.resolve.send(ApprovalOutcome { allow:false, decision:ApprovalDecision::Decline, reason:Some(NO_SUBSCRIBER_REASON.into()) });
        }
        receiver
    }
    pub fn resolve_response(&mut self, response: &Value) -> bool {
        let Some(id) = response["id"].as_u64() else { return false; };
        let Some(pending) = self.pending.remove(&id) else { return false; };
        let decision = match response["result"]["decision"].as_str() {
            Some("accept") => ApprovalDecision::Accept,
            Some("acceptForSession") => ApprovalDecision::AcceptForSession,
            Some("decline") => ApprovalDecision::Decline,
            _ => ApprovalDecision::Cancel,
        };
        if decision == ApprovalDecision::AcceptForSession && let Some(key) = pending.allow_key { self.session_allows.insert(key); }
        (self.send)(&pending.thread_id, json!({"method":"serverRequest/resolved","params":{"threadId":pending.thread_id,"requestId":id}}));
        let allow = matches!(decision, ApprovalDecision::Accept | ApprovalDecision::AcceptForSession);
        let result_reason = response["result"].get("reason").filter(|value| !value.is_null()).or_else(|| response["result"].get("message"));
        let reason = result_reason.and_then(Value::as_str).or_else(|| response["error"]["message"].as_str()).filter(|reason| !reason.is_empty());
        let _delivery = pending.resolve.send(ApprovalOutcome { allow, decision, reason:if allow { None } else { reason.map(str::to_owned) } });
        true
    }
    pub fn replay_pending_for_thread(&self, thread_id: &str) -> usize {
        let mut replayed = 0;
        for pending in self.pending.values().filter(|pending| pending.thread_id == thread_id) { (self.send)(thread_id, pending.request.clone()); replayed += 1; }
        replayed
    }
    pub fn cancel_pending_for_thread(&mut self, thread_id: &str) -> usize {
        let ids = self.pending.iter().filter(|(_, pending)| pending.thread_id == thread_id).map(|(id, _)| *id).collect::<Vec<_>>();
        for id in &ids {
            if let Some(pending) = self.pending.remove(id) {
                (self.send)(thread_id, json!({"method":"serverRequest/resolved","params":{"threadId":thread_id,"requestId":id}}));
                let _delivery = pending.resolve.send(ApprovalOutcome { allow:false, decision:ApprovalDecision::Cancel, reason:Some(CANCEL_REASON.into()) });
            }
        }
        ids.len()
    }
}
