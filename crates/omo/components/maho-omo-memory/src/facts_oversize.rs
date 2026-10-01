use memory_core::facts::{FactsFailureReason,FactsFailureTarget,FactsPayloadEnvelope,FactsQueueEntry,MAX_FACTS_PAYLOAD_BYTES,measure_facts_payload_bytes};
use crate::facts_failure_recording::{preflight_failure_id,queue_entry_targets};
pub trait FactsPreflightFailure {type Error;fn preflight_fail(&mut self,targets:Vec<FactsFailureTarget>,failure_id:String,reason:FactsFailureReason,detail:String)->Result<(),Self::Error>;}
pub fn conversation_frontiers(entries:&[FactsQueueEntry])->Vec<FactsQueueEntry>{
    let mut frontiers:Vec<FactsQueueEntry>=Vec::new();
    for entry in entries{if let Some(current)=frontiers.iter_mut().find(|current|current.conversation_id==entry.conversation_id){if entry.range.end_snapshot_line>current.range.end_snapshot_line{*current=entry.clone();}}else{frontiers.push(entry.clone());}}
    frontiers
}
pub struct OversizeClassificationInput<'a>{pub envelope:&'a FactsPayloadEnvelope,pub oversized:&'a [FactsQueueEntry],pub pending:&'a [FactsQueueEntry],pub envelope_oversized:bool,pub create_failure_id:Option<&'a dyn Fn()->String>,pub max_bytes:Option<usize>}
pub fn classify_oversize_payload<T:FactsPreflightFailure>(terminal:&mut T,input:&OversizeClassificationInput<'_>,warn:&mut dyn FnMut(&str,serde_json::Value))->Result<bool,T::Error>{
    let max_bytes=input.max_bytes.unwrap_or(MAX_FACTS_PAYLOAD_BYTES);
    if input.envelope_oversized{
        let bytes=measure_facts_payload_bytes(&input.envelope.to_payload(vec![]));
        let frontiers=conversation_frontiers(input.pending);
        warn("facts payload envelope exceeds the byte cap; nothing can be launched",serde_json::json!({"envelopeBytes":bytes,"maxBytes":max_bytes,"conversations":frontiers.len()}));
        terminal.preflight_fail(queue_entry_targets(&frontiers),preflight_failure_id(input.create_failure_id),FactsFailureReason::PayloadEnvelopeOversize,format!("facts payload envelope is {bytes} bytes, above the {max_bytes}-byte cap"))?;
        return Ok(true);
    }
    for entry in input.oversized{
        let bytes=measure_facts_payload_bytes(&input.envelope.to_payload(vec![entry.clone()]));
        warn("facts queue entry exceeds the payload byte cap; parking it",serde_json::json!({"conversationId":entry.conversation_id,"endMessageId":entry.range.end_message_id,"entryBytes":bytes,"maxBytes":max_bytes}));
        terminal.preflight_fail(queue_entry_targets(std::slice::from_ref(entry)),preflight_failure_id(input.create_failure_id),FactsFailureReason::PayloadEntryOversize,format!("facts entry payload is {bytes} bytes, above the {max_bytes}-byte cap"))?;
    }
    Ok(false)
}
#[cfg(test)]
mod tests{
    use super::*;
    fn entry(id:&str,line:u64)->FactsQueueEntry{serde_json::from_value(serde_json::json!({"version":1,"identity":"agent","sessionId":"session","conversationId":id,"range":{"start_message_id":"m0","end_message_id":format!("m{line}"),"start_line":0,"end_snapshot_line":line},"enqueuedAt":"2026-08-10T00:00:00Z","entries":[]})).unwrap()}
    struct Recorder(Vec<(Vec<FactsFailureTarget>,FactsFailureReason)>);
    impl FactsPreflightFailure for Recorder{type Error=();fn preflight_fail(&mut self,targets:Vec<FactsFailureTarget>,_:String,reason:FactsFailureReason,_:String)->Result<(),()>{self.0.push((targets,reason));Ok(())}}
    #[test]fn newest_frontier_preserves_first_conversation_order(){let entries=[entry("a",1),entry("b",2),entry("a",3),entry("b",1)];assert_eq!(conversation_frontiers(&entries),[entry("a",3),entry("b",2)]);}
    #[test]fn envelope_refuses_launch_and_charges_frontiers(){let envelope=FactsPayloadEnvelope{version:1,identity:"agent".into(),today:"2026-08-10".into(),known_people:vec![],primary_human:memory_core::facts::FactsPrimaryHuman{slug:"human".into(),aliases:vec![]}};let entries=[entry("a",1),entry("a",3),entry("b",2)];let mut recorder=Recorder(vec![]);assert!(classify_oversize_payload(&mut recorder,&OversizeClassificationInput{envelope:&envelope,oversized:&entries,pending:&entries,envelope_oversized:true,create_failure_id:Some(&||"id".into()),max_bytes:Some(1)},&mut |_,_|{}).unwrap());assert_eq!(recorder.0.len(),1);assert_eq!(recorder.0[0].0.len(),2);assert_eq!(recorder.0[0].0[0].end_snapshot_line,3);assert_eq!(recorder.0[0].1,FactsFailureReason::PayloadEnvelopeOversize);}
    #[test]fn entry_oversize_records_each_without_global_refusal(){let envelope=FactsPayloadEnvelope{version:1,identity:"agent".into(),today:"2026-08-10".into(),known_people:vec![],primary_human:memory_core::facts::FactsPrimaryHuman{slug:"human".into(),aliases:vec![]}};let entries=[entry("a",1),entry("b",2)];let mut recorder=Recorder(vec![]);assert!(!classify_oversize_payload(&mut recorder,&OversizeClassificationInput{envelope:&envelope,oversized:&entries,pending:&entries,envelope_oversized:false,create_failure_id:None,max_bytes:Some(1)},&mut |_,_|{}).unwrap());assert_eq!(recorder.0.len(),2);assert!(recorder.0.iter().all(|(_,reason)|*reason==FactsFailureReason::PayloadEntryOversize));}
}
