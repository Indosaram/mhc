use std::{collections::BTreeMap, path::Path};
use serde_json::{Value, json};
use crate::{sdk_boundary::SdkQueryHandle, session_registry_pump::{PumpEntry, TurnResult, DEFAULT_PRE_REPLAY_MAX_BYTES, DEFAULT_PRE_REPLAY_MAX_MESSAGES}, session_registry_state::{SessionState, transition_to_closing, transition_to_closed}};
pub const SESSION_REGISTRY_IDLE_TTL_MS: u64 = 30 * 60_000;
pub const SESSION_REGISTRY_MAX_ENTRIES: usize = 32;
pub struct SessionEntry {
    pub session_id: String,
    pub account_name: String,
    pub model_id: String,
    pub system_prompt_hash: String,
    pub toolset_hash: String,
    pub pump: PumpEntry,
    query: Option<SdkQueryHandle>,
    pub sent_hashes: Vec<String>,
    pub assistant_uuid_by_index: BTreeMap<usize, String>,
    pub last_used_at: u64,
    pub pending_fork_reason: Option<String>,
    pub tainted_reason: Option<String>,
    pub branch_info: Option<crate::session_entry_annotations::SessionBranchInfo>,
}
pub struct CreateEntry<'a> {
    pub session_id: &'a str, pub sdk_session_id: &'a str,
    pub account_name: &'a str, pub model_id: &'a str,
    pub system_prompt_hash: &'a str, pub toolset_hash: &'a str,
    pub executable: &'a Path, pub options: &'a Value,
    pub environment: &'a BTreeMap<String, String>, pub generation: u64, pub now: u64,
    pub resume_at: Option<Option<&'a str>>,
}
impl SessionEntry {
    pub async fn create(input: CreateEntry<'_>) -> anyhow::Result<Self> {
        let mut options = input.options.clone();
        if let Some(at) = input.resume_at {
            options.as_object_mut().expect("query options").remove("sessionId");
            options["resume"] = json!(input.sdk_session_id);
            if let Some(at) = at { options["resumeSessionAt"] = json!(at); options["forkSession"] = json!(true); }
        } else { options["sessionId"] = json!(input.sdk_session_id); }
        if !options["extraArgs"].is_object() { options["extraArgs"] = json!({}); }
        options["extraArgs"]["replay-user-messages"] = json!("");
        let query = SdkQueryHandle::spawn(input.executable, &options, input.environment).await?;
        Ok(Self { session_id: input.session_id.into(), account_name: input.account_name.into(), model_id: input.model_id.into(), system_prompt_hash: input.system_prompt_hash.into(), toolset_hash: input.toolset_hash.into(),
            pump: PumpEntry { sdk_session_id: input.sdk_session_id.into(), sdk_session_id_confirmed: input.resume_at.is_some(), generation: input.generation, state: SessionState::Starting, active_turn: None }, query: Some(query), sent_hashes: Vec::new(), assistant_uuid_by_index: BTreeMap::new(), last_used_at: input.now, pending_fork_reason: None, tainted_reason: None, branch_info: None })
    }
    pub fn evictable(&self) -> bool { matches!(self.pump.state, SessionState::IdleSynced | SessionState::Tainted) && self.pump.active_turn.is_none() }
    pub fn idle_expired(&self, now: u64) -> bool { now.saturating_sub(self.last_used_at) >= SESSION_REGISTRY_IDLE_TTL_MS }
    pub fn record_synced_stream(&mut self, hashes: &[String]) { self.sent_hashes = hashes.to_vec(); }
    pub async fn set_model(&mut self, model: &str, now: u64) -> anyhow::Result<()> {
        self.query.as_ref().ok_or_else(|| anyhow::anyhow!("Session closed"))?.set_model(model).await?;
        self.model_id = model.into(); self.last_used_at = now; Ok(())
    }
    pub async fn turn(&mut self, uuid: String, message: Value, now: u64, signal: Option<&maho_ai::utils::abort::AbortSignal>, mut deliver: impl FnMut(Value)) -> anyhow::Result<TurnResult> {
        let frame = self.pump.submit(&self.session_id, uuid, message, DEFAULT_PRE_REPLAY_MAX_MESSAGES, DEFAULT_PRE_REPLAY_MAX_BYTES)?;
        self.last_used_at = now;
        let outcome: anyhow::Result<TurnResult> = async {
            let query = self.query.as_mut().ok_or_else(|| anyhow::anyhow!("Session closed"))?;
            query.send(frame).await?;
            let mut abort_deadline = None;
            loop {
                let message = if let Some(deadline) = abort_deadline {
                    match tokio::time::timeout_at(deadline, query.next()).await {
                        Ok(message) => message,
                        Err(_) => return Ok(self.pump.abort_uncertain(true).expect("active aborted turn")),
                    }
                } else if let Some(signal) = signal {
                    tokio::select! {
                        message = query.next() => message,
                        () = signal.cancelled() => {
                            self.pump.active_turn.as_mut().expect("active turn").aborted = true;
                            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(crate::session_registry_pump::SESSION_TURN_ABORT_GRACE_MS);
                            match tokio::time::timeout_at(deadline, query.interrupt()).await {
                                Ok(Ok(receipt)) => self.pump.active_turn.as_mut().expect("active turn").interrupt_receipt = Some(receipt),
                                _ => return Ok(self.pump.abort_uncertain(true).expect("active aborted turn")),
                            }
                            abort_deadline = Some(deadline);
                            continue;
                        }
                    }
                } else { query.next().await };
                let Some(message) = message else { anyhow::bail!("Anthropic Subscription query ended before the active turn result"); };
                let output = self.pump.handle(message?, true)?;
                for message in output.delivered { deliver(message); }
                if let Some(completion) = output.completion {
                    return Ok(completion);
                }
            }
        }.await;
        if outcome.is_err() || outcome.as_ref().is_ok_and(|result| result.aborted && self.pump.state != SessionState::IdleSynced) { self.close().await?; }
        outcome
    }
    pub async fn close(&mut self) -> anyhow::Result<()> {
        if self.query.is_none() { return Ok(()); }
        transition_to_closing(&mut self.pump.state)?;
        self.pump.active_turn = None;
        let result = self.query.take().expect("query").close().await;
        transition_to_closed(&mut self.pump.state)?; result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn resident_query_reuses_child_and_attributes_two_turns() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("dir"); let executable = dir.path().join("claude");
        std::fs::write(&executable,"#!/usr/bin/python3\nimport sys,json\nfor line in sys.stdin:\n f=json.loads(line)\n if f['type']=='control_request':\n  print(json.dumps({'type':'control_response','response':{'subtype':'success','request_id':f['request_id'],'response':{}}}),flush=True)\n else:\n  print(json.dumps({'type':'user','isReplay':True,'uuid':f['uuid']}),flush=True)\n  print(json.dumps({'type':'result','subtype':'success','user_message_uuid':f['uuid'],'result':f['message']['content']}),flush=True)\n").expect("script");
        std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
        let options = json!({}); let environment = BTreeMap::new();
        let mut entry = SessionEntry::create(CreateEntry { session_id:"s",sdk_session_id:"sdk",account_name:"default",model_id:"test",system_prompt_hash:"prompt",toolset_hash:"tools",executable:&executable,options:&options,environment:&environment,generation:1,now:0,resume_at:None }).await.expect("create");
        let mut bindings = crate::session_reattach::BindingStore::default();
        for uuid in ["first","second"] {
            let mut delivered = Vec::new();
            let hashes = vec![uuid.into()];
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), crate::session_turn_attempt::run(&mut entry,uuid.into(),json!({"role":"user","content":uuid}),&hashes,(1,None),&mut bindings,|value|delivered.push(value))).await.expect("bounded turn").expect("turn");
            assert_eq!(result.uuid,uuid); assert_eq!(delivered.len(),1); assert_eq!(delivered[0]["result"],uuid); assert!(entry.evictable());
            assert_eq!(entry.sent_hashes,hashes);assert_eq!(bindings.get("s").expect("retained binding").sent_hashes,hashes);
        }
        assert!(entry.pump.sdk_session_id_confirmed); assert!(!entry.idle_expired(2)); assert!(entry.idle_expired(SESSION_REGISTRY_IDLE_TTL_MS+1));
        assert!(!crate::session_entry_annotations::switch_entry_model(None,"next",2).await.expect("missing"));
        assert!(crate::session_entry_annotations::switch_entry_model(Some(&mut entry),"next",2).await.expect("model")); assert_eq!(entry.model_id,"next");
        crate::session_entry_annotations::annotate_pending_fork(Some(&mut entry),"compaction",3);
        assert_eq!(entry.pending_fork_reason.as_deref(),Some("compaction"));assert_eq!(entry.last_used_at,3);
        let branch=crate::session_entry_annotations::SessionBranchInfo {old_leaf_id:"old".into(),new_leaf_id:"new".into()};
        crate::session_entry_annotations::annotate_branch_info(Some(&mut entry),&branch,4);assert_eq!(entry.branch_info,Some(branch));
        crate::session_entry_annotations::annotate_tainted(Some(&mut entry),"rewritten",5).expect("taint");assert_eq!(entry.pump.state,SessionState::Tainted);
        crate::session_entry_annotations::annotate_tainted(Some(&mut entry),"rewritten",6).expect("idempotent taint");assert_eq!(entry.last_used_at,6);
        entry.close().await.expect("close"); assert_eq!(entry.pump.state,SessionState::Closed);
    }
}
