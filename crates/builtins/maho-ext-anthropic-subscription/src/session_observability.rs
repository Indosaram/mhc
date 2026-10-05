use std::collections::VecDeque;
use crate::session_continuity::sanitize_reason;
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct Observation {pub kind:String,pub reason:String,pub delta_messages:usize,pub payload_bytes:Option<usize>,pub collapsed_directives:Option<usize>}
#[derive(Default)]
pub struct PendingCloseCauses {causes:VecDeque<(String,String)>}
impl PendingCloseCauses {
    pub fn record(&mut self,session:&str,reason:&str)->String {
        self.causes.retain(|(id,_)|id!=session);if self.causes.len()>=256 {self.causes.pop_front();}let cause=sanitize_reason(reason);self.causes.push_back((session.into(),cause.clone()));cause
    }
    pub fn peek(&self,session:&str)->Option<&str> {self.causes.iter().find(|(id,_)|id==session).map(|(_,cause)|cause.as_str())}
    pub fn consume(&mut self,session:&str)->Option<String> {let index=self.causes.iter().position(|(id,_)|id==session)?;self.causes.remove(index).map(|(_,reason)|reason)}
}
pub struct SyncObservationInput<'a> {pub kind:&'a str,pub reason:Option<&'a str>,pub delta_messages:usize,pub first_turn:bool,pub session:&'a str,pub payload_bytes:Option<usize>,pub collapsed_directives:Option<usize>}
pub fn observe(input:SyncObservationInput<'_>,pending:&mut PendingCloseCauses)->Observation {
    let (kind,reason)=match input.kind {
        "incremental"=>("delta","prefix_matched".into()),
        "resume"=> {let cause=if input.reason==Some("registry_miss") {pending.consume(input.session)}else {None};("fork",cause.unwrap_or_else(||input.reason.map(sanitize_reason).unwrap_or_else(||"branch_resume".into())))},
        _=>{let reason=if input.reason==Some("registry_miss") {pending.peek(input.session).map(str::to_owned)}else {None}.unwrap_or_else(||sanitize_reason(input.reason.unwrap_or_default()));(if input.first_turn&&reason=="registry_miss" {"bootstrap"}else {"flatten"},reason)},
    };
    let cold=input.kind=="cold-seed";Observation {kind:kind.into(),reason,delta_messages:input.delta_messages,payload_bytes:if cold {input.payload_bytes}else {None},collapsed_directives:if cold {input.collapsed_directives}else {None}}
}
pub struct StagedDecision {pub observation:Observation,emitted:bool}
impl StagedDecision {
    pub fn new(observation:Observation)->Self {Self {observation,emitted:false}}
    pub fn emit(&mut self,before:impl FnOnce(),mut on_decision:impl FnMut(&Observation),mut boundary:impl FnMut(&Observation)) {
        if self.emitted {return;}self.emitted=true;before();on_decision(&self.observation);boundary(&self.observation);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_causes_are_fifo_bounded_and_rerecord_moves_to_back() {
        let mut pending=PendingCloseCauses::default();for n in 0..256 {pending.record(&n.to_string(),"compaction");}pending.record("0","tainted:abort");pending.record("256","fork");assert!(pending.peek("1").is_none());assert_eq!(pending.consume("0").as_deref(),Some("tainted_abort"));assert!(pending.peek("0").is_none());
    }
    #[test]
    fn cold_seed_peeks_until_retained_emit_and_emit_is_one_shot() {
        let mut pending=PendingCloseCauses::default();pending.record("s","interrupt failed");let observation=observe(SyncObservationInput {kind:"cold-seed",reason:Some("registry_miss"),delta_messages:2,first_turn:true,session:"s",payload_bytes:Some(42),collapsed_directives:None},&mut pending);assert_eq!(observation.reason,"abort_timeout");assert_eq!(pending.peek("s"),Some("abort_timeout"));
        let mut staged=StagedDecision::new(observation);let mut emitted=0;staged.emit(||{pending.consume("s");},|_|{},|_|emitted+=1);staged.emit(||panic!("already emitted"),|_|{},|_|emitted+=1);assert_eq!(emitted,1);assert!(pending.peek("s").is_none());
    }
}
