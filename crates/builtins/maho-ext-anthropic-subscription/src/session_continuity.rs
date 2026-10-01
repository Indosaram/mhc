use std::collections::BTreeMap;
use sha2::{Digest,Sha256};
#[derive(Clone,Default)]
pub struct Snapshot {
    pub sdk_session_id:String,pub account_name:String,pub model_id:String,pub system_prompt_hash:String,pub toolset_hash:String,
    pub sent_count:usize,pub sent_hashes:Vec<String>,pub last_assistant_uuid:Option<String>,pub assistant_uuid_by_index:BTreeMap<usize,String>,
    pub pending_fork_reason:Option<String>,pub tainted_reason:Option<String>,pub sent_prefix_hash:Option<String>,pub unanswered_turn_digest:Option<String>,pub sdk_session_id_confirmed:Option<bool>,
}
pub struct ContinuityInput<'a> {
    pub entry:Option<&'a Snapshot>,pub binding:Option<&'a Snapshot>,pub current_hashes:&'a [String],pub account_name:&'a str,pub model_id:&'a str,
    pub system_prompt_hash:&'a str,pub toolset_hash:&'a str,pub transcript_available:bool,pub cross_account_resume_supported:bool,pub idle_expired:bool,pub invalidation_reason:Option<&'a str>,
}
#[derive(Debug,PartialEq,Eq)]
pub enum Decision {Bootstrap {reason:Option<String>},Delta {from:usize},Reattach {sdk_session_id:String,from:usize,reason:String},Fork {sdk_session_id:String,at_uuid:String,from:usize,reason:String},Flatten {reason:String}}
pub fn prefix_digest(hashes:&[String],count:usize)->String {format!("{:x}",Sha256::digest(serde_json::to_vec(&hashes[..count.min(hashes.len())]).expect("string array serialization")))}
fn common_prefix(left:&[String],right:&[String])->usize {left.iter().zip(right).take_while(|(l,r)|l==r).count()}
fn flatten(reason:&str)->Decision {Decision::Flatten {reason:reason.into()}}
fn reattach(entry:&Snapshot,reason:&str)->Decision {Decision::Reattach {sdk_session_id:entry.sdk_session_id.clone(),from:entry.sent_count,reason:reason.into()}}
fn fork(entry:&Snapshot,count:usize,reason:&str)->Decision {
    if count<=1 {return flatten(reason);}
    let boundary=entry.assistant_uuid_by_index.range(1..count).next_back();
    match boundary {Some((index,uuid))=>Decision::Fork {sdk_session_id:entry.sdk_session_id.clone(),at_uuid:uuid.clone(),from:*index,reason:reason.into()},None=>flatten(reason)}
}
fn drift(input:&ContinuityInput<'_>,entry:&Snapshot)->Option<&'static str> {
    if entry.account_name!=input.account_name {Some("account_changed")}else if entry.model_id!=input.model_id {Some("model_changed")}else if entry.system_prompt_hash!=input.system_prompt_hash {Some("system_prompt_changed")}else if entry.toolset_hash!=input.toolset_hash {Some("toolset_changed")}else {None}
}
fn from_binding(input:&ContinuityInput<'_>,binding:&Snapshot)->Decision {
    if !input.transcript_available {return flatten("transcript_missing");}
    let reason=drift(input,binding);
    if reason==Some("model_changed") {return flatten("model_changed");}
    if reason==Some("account_changed")&&!input.cross_account_resume_supported {return flatten("cross_root_unsupported");}
    let shared=common_prefix(&binding.sent_hashes,input.current_hashes);
    let matches_prefix=||binding.sent_prefix_hash.as_ref().map_or(shared==binding.sent_count,|hash|prefix_digest(input.current_hashes,binding.sent_count)==*hash);
    if binding.unanswered_turn_digest.as_ref().is_some_and(|hash|prefix_digest(input.current_hashes,input.current_hashes.len())==*hash)&&input.current_hashes.len()>=binding.sent_count&&matches_prefix() {
        return match &binding.last_assistant_uuid {Some(uuid)=>Decision::Fork {sdk_session_id:binding.sdk_session_id.clone(),at_uuid:uuid.clone(),from:binding.sent_count,reason:"timeout_retry".into()},None=>flatten("timeout_retry")};
    }
    if binding.sent_prefix_hash.is_some() {
        if input.current_hashes.len()>=binding.sent_count&&matches_prefix() {return reattach(binding,reason.unwrap_or("registry_miss"));}
        return flatten(if input.current_hashes.len()<binding.sent_count {"history_rolled_back"}else {"sent_stream_diverged"});
    }
    if shared==binding.sent_count {return reattach(binding,reason.unwrap_or("registry_miss"));}
    match &binding.last_assistant_uuid {None=>flatten("registry_miss"),Some(uuid)=>Decision::Fork {sdk_session_id:binding.sdk_session_id.clone(),at_uuid:uuid.clone(),from:shared,reason:if shared<binding.sent_count {"history_rolled_back"}else {"sent_stream_diverged"}.into()}}
}
pub fn decide(input:&ContinuityInput<'_>)->Decision {
    let mut decision=if let Some(entry)=input.entry {
        if let Some(divergence)=entry.pending_fork_reason.as_deref().or(entry.tainted_reason.as_deref()) {fork(entry,entry.sent_count,match divergence {"assistant_rewritten"=>"assistant_rewritten","compaction"=>"tainted_compaction",_=>"other"})}
        else {let shared=common_prefix(&entry.sent_hashes,input.current_hashes);
            if shared<entry.sent_count {let rolled_back=input.current_hashes.len()<entry.sent_count&&shared==input.current_hashes.len();fork(entry,if rolled_back {input.current_hashes.len()}else {shared+1},if rolled_back {"history_rolled_back"}else {"sent_stream_diverged"})}
            else if input.idle_expired {reattach(entry,"idle_ttl")}
            else if let Some(reason)=drift(input,entry) {reattach(entry,reason)}else {Decision::Delta {from:entry.sent_count}}
        }
    } else if let Some(binding)=input.binding {let decision=from_binding(input,binding);if binding.sdk_session_id_confirmed==Some(false)&&matches!(decision,Decision::Reattach {..}|Decision::Fork {..}) {flatten("session_unconfirmed")}else {decision}}
    else {Decision::Bootstrap {reason:None}};
    if let Some(reason)=input.invalidation_reason {let cause=match reason {"compaction"=>"tainted_compaction","tree_changed"=>"branch_diverged","fork"=>"tainted_fork",other=>sanitize_reason(other)};
        match &mut decision {Decision::Bootstrap {reason}=>*reason=Some(cause.into()),Decision::Flatten {reason} if reason=="registry_miss"=>*reason=cause.into(),_=>{}}
    }
    decision
}
pub fn sanitize_reason(reason:&str)->&str {
    match reason {
        "prefix_matched"|"registry_miss"|"idle_ttl"|"capacity"|"model_selected"|"thinking_level_selected"|"bound_account_token_expiring"|"account_changed"|"model_changed"|"toolset_changed"|"system_prompt_changed"|"assistant_stream_diverged"|"options_changed"|"history_rolled_back"|"assistant_rewritten"|"transcript_missing"|"cross_root_unsupported"|"sent_stream_diverged"|"branch_diverged"|"branch_boundary_unavailable"|"branch_resume"|"tainted_compaction"|"tainted_fork"|"tainted_abort"|"tainted_assistant_provenance_unverified"|"resume_initialization_failed"|"resume_initialization_aborted"|"resume_mode_off"|"query_failed"|"turn_attribution_failed"|"session_unconfirmed"|"abort_timeout"|"extensions_removed"|"session_shutdown"|"timeout_retry"|"other"=>reason,
        _=>"other",
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot()->Snapshot {Snapshot {sdk_session_id:"sdk".into(),account_name:"primary".into(),model_id:"model".into(),system_prompt_hash:"prompt".into(),toolset_hash:"tools".into(),sent_count:2,sent_hashes:vec!["h1".into(),"h2".into()],last_assistant_uuid:Some("a2".into()),assistant_uuid_by_index:[(1,"a1".into()),(2,"a2".into())].into(),..Default::default()}}
    fn input<'a>(hashes:&'a [String])->ContinuityInput<'a> {ContinuityInput {entry:None,binding:None,current_hashes:hashes,account_name:"primary",model_id:"model",system_prompt_hash:"prompt",toolset_hash:"tools",transcript_available:true,cross_account_resume_supported:true,idle_expired:false,invalidation_reason:None}}
    #[test]
    fn live_delta_drift_and_strict_boundary() {
        let mut entry=snapshot();let hashes=vec!["h1".into(),"h2".into(),"h3".into()];let mut request=input(&hashes);request.entry=Some(&entry);assert_eq!(decide(&request),Decision::Delta {from:2});request.model_id="other";assert_eq!(decide(&request),reattach(&entry,"model_changed"));
        entry.pending_fork_reason=Some("assistant_rewritten".into());let mut request=input(&hashes);request.entry=Some(&entry);assert_eq!(decide(&request),Decision::Fork {sdk_session_id:"sdk".into(),at_uuid:"a1".into(),from:1,reason:"assistant_rewritten".into()});assert_eq!(fork(&entry,0,"history_rolled_back"),flatten("history_rolled_back"));
    }
    #[test]
    fn restored_retry_checkpoint_and_confirmation() {
        let hashes=vec!["h1".into(),"h2".into(),"h3".into()];let mut binding=snapshot();binding.sent_prefix_hash=Some(prefix_digest(&hashes,2));binding.unanswered_turn_digest=Some(prefix_digest(&hashes,3));let mut request=input(&hashes);request.binding=Some(&binding);
        assert_eq!(decide(&request),Decision::Fork {sdk_session_id:"sdk".into(),at_uuid:"a2".into(),from:2,reason:"timeout_retry".into()});binding.sdk_session_id_confirmed=Some(false);let mut request=input(&hashes);request.binding=Some(&binding);assert_eq!(decide(&request),flatten("session_unconfirmed"));request.transcript_available=false;assert_eq!(decide(&request),flatten("transcript_missing"));
    }
    #[test]
    fn restored_prefix_fails_closed_and_invalidations_are_sanitized() {
        let hashes=vec!["h1".into(),"different".into(),"h3".into()];let mut binding=snapshot();binding.sent_prefix_hash=Some(prefix_digest(&binding.sent_hashes,2));let mut request=input(&hashes);request.binding=Some(&binding);assert_eq!(decide(&request),flatten("sent_stream_diverged"));request.binding=None;request.invalidation_reason=Some("arbitrary credential text");assert_eq!(decide(&request),Decision::Bootstrap {reason:Some("other".into())});request.invalidation_reason=Some("compaction");assert_eq!(decide(&request),Decision::Bootstrap {reason:Some("tainted_compaction".into())});
    }
}
