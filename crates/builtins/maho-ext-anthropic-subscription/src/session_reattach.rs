use std::collections::BTreeMap;
use serde_json::Value;
use crate::session_continuity::Snapshot;
#[derive(Default)]
pub struct BindingStore {bindings:BTreeMap<String,Snapshot>,invalidations:BTreeMap<String,String>}
impl BindingStore {
    pub fn remember(&mut self,session:&str,binding:&Snapshot) {self.bindings.insert(session.into(),binding.clone());}
    pub fn get(&self,session:&str)->Option<Snapshot> {self.bindings.get(session).cloned()}
    pub fn forget(&mut self,session:&str) {self.bindings.remove(session);}
    pub fn remember_invalidation(&mut self,session:&str,reason:Option<&str>) {if let Some(reason)=reason {self.invalidations.insert(session.into(),reason.into());}else {self.invalidations.remove(session);}}
    pub fn invalidation_reason(&self,session:&str)->Option<&str> {self.invalidations.get(session).map(String::as_str)}
}
pub fn abort_keeps_query(receipt:&Value)->bool {receipt["still_queued"].as_array().is_some_and(Vec::is_empty)}
pub fn verify_transcript(binding:&Snapshot,messages:&[Value],auth_lane:&str)->bool {
    if auth_lane=="config-dir"||messages.is_empty()||messages.iter().any(|m|m["session_id"]!=binding.sdk_session_id) {return false;}
    let Some(uuid)=binding.last_assistant_uuid.as_ref() else {return true;};
    let Some(anchor)=messages.iter().position(|m|m["type"]=="assistant"&&m["uuid"]==*uuid&&m.get("parent_tool_use_id").is_some_and(Value::is_null)) else {return false;};
    !messages[anchor+1..].iter().any(|m|m["type"]=="user"&&m.get("parent_tool_use_id").is_some_and(Value::is_null))
}
pub fn lineage_options(binding:&Snapshot,at_uuid:Option<&str>,options:&Value)->Value {
    let mut options=options.as_object().expect("query options").clone();options.remove("sessionId");options.insert("resume".into(),Value::String(binding.sdk_session_id.clone()));
    if let Some(uuid)=at_uuid {options.insert("resumeSessionAt".into(),Value::String(uuid.into()));options.insert("forkSession".into(),Value::Bool(true));}Value::Object(options)
}
#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    #[test]
    fn restored_transcript_fails_closed_for_wrong_session_nested_anchor_and_orphan_tail() {
        let binding=Snapshot {sdk_session_id:"sdk".into(),last_assistant_uuid:Some("a".into()),..Default::default()};let anchor=json!({"type":"assistant","uuid":"a","session_id":"sdk","parent_tool_use_id":null});
        assert!(!verify_transcript(&binding,&[],"oauth-slots"));assert!(verify_transcript(&binding,std::slice::from_ref(&anchor),"oauth-slots"));assert!(!verify_transcript(&binding,std::slice::from_ref(&anchor),"config-dir"));
        let mut wrong=anchor.clone();wrong["session_id"]=json!("other");assert!(!verify_transcript(&binding,&[wrong],"ambient"));let mut nested=anchor.clone();nested["parent_tool_use_id"]=json!("tool");assert!(!verify_transcript(&binding,&[nested],"ambient"));
        assert!(!verify_transcript(&binding,&[anchor,json!({"type":"user","session_id":"sdk","parent_tool_use_id":null})],"ambient"));
    }
    #[test]
    fn clones_bindings_and_omits_session_id_on_reattach() {
        let binding=Snapshot {sdk_session_id:"sdk".into(),sent_hashes:vec!["h".into()],..Default::default()};let mut store=BindingStore::default();store.remember("s",&binding);store.get("s").expect("binding").sent_hashes.clear();assert_eq!(store.get("s").expect("binding").sent_hashes.len(),1);
        let options=lineage_options(&binding,Some("a"),&json!({"sessionId":"minted","model":"m"}));assert!(options.get("sessionId").is_none());assert_eq!(options["resume"],"sdk");assert_eq!(options["resumeSessionAt"],"a");assert_eq!(options["forkSession"],true);assert!(abort_keeps_query(&json!({"still_queued":[]})));assert!(!abort_keeps_query(&json!({})));
    }
}
