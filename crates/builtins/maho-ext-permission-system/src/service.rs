use crate::{evaluate::evaluate, events::{PermissionEventEmitter, PermissionRepliedEvent}, types::{Action, PermissionDecision, PermissionError, Reply, ReplyInput, Request, Rule, Ruleset}};
use std::future::Future;
use tokio::sync::oneshot;

struct PendingEntry { info: Request, sender: oneshot::Sender<Result<(), PermissionError>> }
pub struct PermissionService {
    pending: Vec<PendingEntry>, approved: Ruleset, static_ruleset: Ruleset,
    emitter: PermissionEventEmitter, id_counter: u64,
}
impl PermissionService {
    pub fn new(static_ruleset: Ruleset, approved: Ruleset, emitter: PermissionEventEmitter) -> Self {
        Self { pending: Vec::new(), approved, static_ruleset, emitter, id_counter: 0 }
    }
    pub fn ask(&mut self, mut info: Request) -> impl Future<Output=Result<(),PermissionError>> + use<> {
        if info.id.is_empty() { self.id_counter += 1; info.id=format!("permission-{}",self.id_counter); }
        let mut denied=Vec::new(); let mut needs_ask=false;
        for pattern in &info.patterns {
            match evaluate(&info.permission,pattern,&[&self.static_ruleset,&self.approved]).action {
                Action::Deny => denied.push(pattern.clone()), Action::Ask => needs_ask=true, Action::Allow => {},
            }
        }
        let (sender,receiver)=oneshot::channel();
        if !denied.is_empty() {
            if sender.send(Err(PermissionError::Denied(denied))).is_err() { unreachable!("receiver exists"); }
        } else if !needs_ask {
            self.emit_replied(&info,PermissionDecision::Allow);
            if sender.send(Ok(())).is_err() { unreachable!("receiver exists"); }
        } else {
            self.pending.push(PendingEntry { info: info.clone(), sender });
            if let Err(error)=self.emitter.emit_asked(&info) { eprintln!("Error in permission_asked handler: {error}"); }
        }
        async move { receiver.await.unwrap_or(Err(PermissionError::Rejected)) }
    }
    fn emit_replied(&self, info: &Request, reply: PermissionDecision) {
        if let Err(error)=self.emitter.emit_replied(PermissionRepliedEvent {request_id:info.id.clone(),session_id:info.session_id.clone(),reply}) { eprintln!("Error in permission_replied handler: {error}"); }
    }
    pub fn reply(&mut self, input: ReplyInput) {
        let Some(index)=self.pending.iter().position(|entry| entry.info.id == input.request_id) else { return };
        let entry=self.pending.remove(index);
        let decision=match input.reply { Reply::Once=>PermissionDecision::Once,Reply::Always=>PermissionDecision::Always,Reply::Reject=>PermissionDecision::Reject };
        self.emit_replied(&entry.info,decision);
        match input.reply {
            Reply::Reject => {
                let error=match input.message.filter(|message| !message.is_empty()) {Some(message)=>PermissionError::Corrected(message),None=>PermissionError::Rejected};
                if entry.sender.send(Err(error)).is_err() { eprintln!("Permission requester dropped before rejection"); }
                let mut index=0;
                while index<self.pending.len() {
                    if self.pending[index].info.session_id == entry.info.session_id {
                        let other=self.pending.remove(index); self.emit_replied(&other.info,PermissionDecision::Reject);
                        if other.sender.send(Err(PermissionError::Rejected)).is_err() { eprintln!("Permission requester dropped before rejection"); }
                    } else { index+=1; }
                }
            }
            Reply::Once => { if entry.sender.send(Ok(())).is_err() { eprintln!("Permission requester dropped before approval"); } }
            Reply::Always => {
                for pattern in &entry.info.always { self.approved.push(Rule {permission:entry.info.permission.clone(),pattern:pattern.clone(),action:Action::Allow}); }
                if entry.sender.send(Ok(())).is_err() { eprintln!("Permission requester dropped before approval"); }
                let mut index=0;
                while index<self.pending.len() {
                    let other=&self.pending[index];
                    if other.info.session_id == entry.info.session_id && other.info.patterns.iter().all(|pattern| evaluate(&other.info.permission,pattern,&[&self.static_ruleset,&self.approved]).action == Action::Allow) {
                        let other=self.pending.remove(index); self.emit_replied(&other.info,PermissionDecision::Always);
                        if other.sender.send(Ok(())).is_err() { eprintln!("Permission requester dropped before approval"); }
                    } else { index+=1; }
                }
            }
        }
    }
    pub fn list(&self) -> Vec<Request> { self.pending.iter().map(|entry| entry.info.clone()).collect() }
    pub fn get_approved(&self) -> Ruleset { self.approved.clone() }
}
