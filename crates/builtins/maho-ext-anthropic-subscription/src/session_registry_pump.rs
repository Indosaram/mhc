use serde_json::Value;
use crate::{session_registry_state::{SessionState,transition_session_state},session_turn_claim::{PreReplayBuffer,is_replay_for,result_matches_turn},errors::sdk_result_failure,refusal::refusal_error};
pub const DEFAULT_PRE_REPLAY_MAX_MESSAGES:usize=64;
pub const DEFAULT_PRE_REPLAY_MAX_BYTES:usize=256*1024;
pub const SESSION_TURN_ABORT_GRACE_MS:u64=1000;
pub struct ActiveTurn {pub uuid:String,pub generation:u64,pub messages:Vec<Value>,pub claimed:bool,pub aborted:bool,pub interrupt_receipt:Option<Value>,buffer:PreReplayBuffer}
pub struct PumpEntry {pub sdk_session_id:String,pub sdk_session_id_confirmed:bool,pub generation:u64,pub state:SessionState,pub active_turn:Option<ActiveTurn>}
#[derive(Debug,PartialEq)]
pub struct TurnResult {pub uuid:String,pub messages:Vec<Value>,pub aborted:bool}
#[derive(Debug,Default,PartialEq)]
pub struct PumpOutput {pub delivered:Vec<Value>,pub completion:Option<TurnResult>,pub close_reason:Option<String>}
impl PumpEntry {
    pub fn submit(&mut self,session:&str,uuid:String,message:Value,max_messages:usize,max_bytes:usize)->anyhow::Result<Value> {
        if self.active_turn.is_some() {anyhow::bail!("Concurrent Anthropic Subscription turn admission for session {session}");}
        if self.state==SessionState::Starting {transition_session_state(&mut self.state,SessionState::IdleSynced)?;}
        transition_session_state(&mut self.state,SessionState::TurnWaiting)?;
        self.active_turn=Some(ActiveTurn {uuid:uuid.clone(),generation:self.generation,messages:Vec::new(),claimed:false,aborted:false,interrupt_receipt:None,buffer:PreReplayBuffer {messages:Vec::new(),bytes:0,max_messages,max_bytes}});
        transition_session_state(&mut self.state,SessionState::TurnSent)?;
        Ok(serde_json::json!({"type":"user","message":message,"parent_tool_use_id":null,"uuid":uuid,"session_id":self.sdk_session_id}))
    }
    fn deliver(&mut self,message:Value,output:&mut PumpOutput)->anyhow::Result<()> {
        if self.state==SessionState::TurnClaimed {transition_session_state(&mut self.state,SessionState::TurnStreaming)?;}
        self.active_turn.as_mut().expect("active turn").messages.push(message.clone());output.delivered.push(message);Ok(())
    }
    fn claim(&mut self)->anyhow::Result<Vec<Value>> {
        let turn=self.active_turn.as_mut().expect("active turn");turn.claimed=true;transition_session_state(&mut self.state,SessionState::TurnClaimed)?;Ok(turn.buffer.claim())
    }
    fn finish(&mut self,message:Value,output:&mut PumpOutput)->anyhow::Result<()> {
        let turn=self.active_turn.as_ref().expect("active turn");
        if !result_matches_turn(&message,&turn.uuid,turn.claimed) {anyhow::bail!("Anthropic Subscription result user_message_uuid did not match the active turn");}
        if self.state==SessionState::TurnClaimed {transition_session_state(&mut self.state,SessionState::TurnStreaming)?;}
        if !self.active_turn.as_ref().expect("turn").aborted {self.deliver(message,output)?;}
        transition_session_state(&mut self.state,SessionState::TurnResultSeen)?;
        let turn=self.active_turn.take().expect("turn");
        let keep=turn.interrupt_receipt.as_ref().and_then(|receipt|receipt["still_queued"].as_array()).is_some_and(Vec::is_empty);
        if !turn.aborted||keep {transition_session_state(&mut self.state,SessionState::IdleSynced)?;}else {output.close_reason=Some("abort_uncertain".into());}
        output.completion=Some(TurnResult {uuid:turn.uuid,messages:turn.messages,aborted:turn.aborted});Ok(())
    }
    pub fn handle(&mut self,message:Value,current_generation:bool)->anyhow::Result<PumpOutput> {
        let mut output=PumpOutput::default();
        if message["type"]=="system"&&message["subtype"]=="init"&&let Some(id)=message["session_id"].as_str() {self.sdk_session_id=id.into();self.sdk_session_id_confirmed=true;}
        let Some(turn)=self.active_turn.as_ref().filter(|turn|current_generation&&turn.generation==self.generation) else {return Ok(output);};
        if !turn.claimed {
            if is_replay_for(&message,&turn.uuid) {self.sdk_session_id_confirmed=true;for buffered in self.claim()? {if buffered["type"]=="result" {self.finish(buffered,&mut output)?;}else {self.deliver(buffered,&mut output)?;}}}
            else if message["type"]=="stream_event" {self.active_turn.as_mut().expect("turn").buffer.push(message,current_generation)?;}
            else if message["type"]=="result" {
                if let Some(failure)=sdk_result_failure(&message) {return Err(failure.into());}
                if !result_matches_turn(&message,&turn.uuid,turn.claimed) {anyhow::bail!("Anthropic Subscription result arrived before replay claim");}
                for buffered in self.claim()? {self.deliver(buffered,&mut output)?;}self.finish(message,&mut output)?;
            }
            return Ok(output);
        }
        if message["type"]=="user"&&message["isReplay"]==true {return Ok(output);}
        if let Some(refusal)=refusal_error(&message) {return Err(refusal.into());}
        if message["type"]=="result" {if let Some(failure)=sdk_result_failure(&message) {return Err(failure.into());}self.finish(message,&mut output)?;}else {self.deliver(message,&mut output)?;}Ok(output)
    }
    pub fn abort_uncertain(&mut self,current_generation:bool)->Option<TurnResult> {
        if !current_generation {return None;}let turn=self.active_turn.take()?;Some(TurnResult {uuid:turn.uuid,messages:turn.messages,aborted:true})
    }
}
#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    fn entry()->PumpEntry {PumpEntry {sdk_session_id:"minted".into(),sdk_session_id_confirmed:false,generation:1,state:SessionState::Starting,active_turn:None}}
    #[test]
    fn replay_claim_releases_buffer_and_matching_result_finishes() {
        let mut entry=entry();entry.submit("s","u".into(),json!({"role":"user","content":"prompt"}),64,262144).expect("submit");assert!(entry.submit("s","v".into(),json!({}),64,262144).is_err());
        assert!(entry.handle(json!({"type":"stream_event","event":{"type":"content_block_delta"}}),true).expect("buffer").delivered.is_empty());
        let claim=entry.handle(json!({"type":"user","uuid":"u","isReplay":true}),true).expect("claim");assert_eq!(claim.delivered.len(),1);assert!(entry.sdk_session_id_confirmed);
        let result=entry.handle(json!({"type":"result","subtype":"success","user_message_uuid":"u"}),true).expect("result");assert_eq!(result.completion.expect("completion").messages.len(),2);assert_eq!(entry.state,SessionState::IdleSynced);
    }
    #[test]
    fn result_only_uuid_is_supported_but_unattributed_success_is_rejected() {
        for uuid in [Some("u"),None] {let mut entry=entry();entry.submit("s","u".into(),json!({}),64,262144).expect("submit");let mut result=json!({"type":"result","subtype":"success"});if let Some(uuid)=uuid {result["user_message_uuid"]=json!(uuid);}let outcome=entry.handle(result,true);assert_eq!(outcome.is_ok(),uuid.is_some());}
    }
    #[test]
    fn init_adopts_fork_id_even_without_active_turn_and_stale_turn_does_not_deliver() {
        let mut entry=entry();entry.handle(json!({"type":"system","subtype":"init","session_id":"fork"}),true).expect("init");assert_eq!(entry.sdk_session_id,"fork");entry.submit("s","u".into(),json!({}),64,262144).expect("submit");assert!(entry.handle(json!({"type":"user","uuid":"u","isReplay":true}),false).expect("stale").delivered.is_empty());assert!(entry.abort_uncertain(false).is_none());assert!(entry.abort_uncertain(true).expect("abort").aborted);
    }
}
