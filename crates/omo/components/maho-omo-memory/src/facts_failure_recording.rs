use serde::{Deserialize,Serialize};
use memory_core::facts::{FactsFailureTarget,FactsQueueEntry,FactsFailuresFile,failures_store::{FactsFailureStore,FactsFailureStoreError,RecordFailureRequest}};
pub trait FactsFailurePort {
    fn record_failure(&self,request:RecordFailureRequest)->Result<FactsFailuresFile,FactsFailureStoreError>;
    fn clear_on_success(&self,targets:&[FactsFailureTarget])->Result<FactsFailuresFile,FactsFailureStoreError>;
}
impl FactsFailurePort for FactsFailureStore {
    fn record_failure(&self,request:RecordFailureRequest)->Result<FactsFailuresFile,FactsFailureStoreError>{FactsFailureStore::record_failure(self,request)}
    fn clear_on_success(&self,targets:&[FactsFailureTarget])->Result<FactsFailuresFile,FactsFailureStoreError>{FactsFailureStore::clear_on_success(self,targets)}
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct FactsQueuedKey {
    #[serde(rename="conversationId")]
    pub conversation_id:String,
    pub end_message_id:String,
    #[serde(default)]
    pub end_snapshot_line:u64,
}
pub fn queue_entry_targets(entries:&[FactsQueueEntry])->Vec<FactsFailureTarget>{entries.iter().map(|entry|FactsFailureTarget{conversation_id:entry.conversation_id.clone(),end_message_id:entry.range.end_message_id.clone(),end_snapshot_line:entry.range.end_snapshot_line}).collect()}
pub fn ledger_targets(queued:&[FactsQueuedKey])->Vec<FactsFailureTarget>{queued.iter().map(|key|FactsFailureTarget{conversation_id:key.conversation_id.clone(),end_message_id:key.end_message_id.clone(),end_snapshot_line:key.end_snapshot_line}).collect()}
pub fn preflight_failure_id(create_id:Option<&dyn Fn()->String>)->String{format!("preflight-{}",create_id.map_or_else(memory_core::support::random::random_uuid,|create|create()))}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]fn legacy_ledger_target_retains_zero_boundary(){let queued:Vec<FactsQueuedKey>=serde_json::from_value(serde_json::json!([{"conversationId":"session","end_message_id":"m1"}])).unwrap();assert_eq!(ledger_targets(&queued),[FactsFailureTarget{conversation_id:"session".into(),end_message_id:"m1".into(),end_snapshot_line:0}]);}
    #[test]fn injected_preflight_identifier_keeps_namespace(){assert_eq!(preflight_failure_id(Some(&||"id".into())),"preflight-id");assert_ne!(preflight_failure_id(None),preflight_failure_id(None));}
}
