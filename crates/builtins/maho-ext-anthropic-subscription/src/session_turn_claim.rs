use serde_json::Value;
pub fn is_replay_for(message:&Value,uuid:&str)->bool {message["type"]=="user"&&message["isReplay"]==true&&message["uuid"]==uuid}
pub fn is_autonomous_result(message:&Value)->bool {
    (message.get("origin").is_some_and(|origin|!origin.is_null()&&origin["kind"]!="human"))
        ||["parent_tool_use_id","subagent_type"].iter().any(|key|message.get(*key).is_some_and(|value|!value.is_null()))||message["isSynthetic"]==true
}
/// `isForeignResult`: a result that belongs to another turn Claude Code is running, not to this one.
pub fn is_foreign_result(message:&Value,uuid:&str)->bool {
    if let Some(value)=message.get("user_message_uuid") {return value!=uuid;}
    is_autonomous_result(message)
}
pub fn result_matches_turn(message:&Value,uuid:&str,claimed:bool)->bool {
    if let Some(value)=message.get("user_message_uuid") {return value==uuid;}
    claimed&&!is_autonomous_result(message)
}
/// Claude Code echoes a submitted prompt before that turn's first API call, so events ahead of the replay
/// belong to a turn it is already running (an autonomous turn or a background subagent). Only main-thread
/// events are held, as a bounded courtesy; a segment that outgrows the caps is another turn's and is
/// dropped until that turn ends (senpi#2192).
pub struct PreReplayBuffer {pub messages:Vec<Value>,pub bytes:usize,pub max_messages:usize,pub max_bytes:usize,pub overflowed:bool}
impl PreReplayBuffer {
    pub fn push(&mut self,message:Value,current_generation:bool)->anyhow::Result<()> {
        if message.get("parent_tool_use_id").is_some_and(|parent|!parent.is_null())||self.overflowed {return Ok(());}
        self.bytes+=serde_json::to_vec(&message)?.len();self.messages.push(message);
        if self.messages.len()>self.max_messages||self.bytes>self.max_bytes {self.drop_all();self.overflowed=true;return Ok(());}
        if !current_generation {self.messages.clear();}Ok(())
    }
    /// `endForeignTurnBeforeReplay`: the foreign turn ended, so the next events may be ours again.
    pub fn end_foreign_turn(&mut self) {self.drop_all();self.overflowed=false;}
    fn drop_all(&mut self) {self.messages.clear();self.bytes=0;}
    pub fn claim(&mut self)->Vec<Value> {self.bytes=0;std::mem::take(&mut self.messages)}
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn exact_uuid_and_autonomous_exclusion() {
        assert!(is_replay_for(&json!({"type":"user","isReplay":true,"uuid":"u"}),"u"));
        assert!(!result_matches_turn(&json!({"user_message_uuid":"other"}),"u",true));
        for message in [json!({"origin":{"kind":"agent"}}),json!({"parent_tool_use_id":"x"}),json!({"subagent_type":"worker"}),json!({"isSynthetic":true})] {assert!(!result_matches_turn(&message,"u",true));}
        assert!(result_matches_turn(&json!({"origin":{"kind":"human"}}),"u",true));assert!(!result_matches_turn(&json!({}),"u",false));
    }
    #[test]
    fn buffer_counts_utf8_and_enforces_both_limits() {
        let mut buffer=PreReplayBuffer {messages:Vec::new(),bytes:0,max_messages:1,max_bytes:100,overflowed:false};buffer.push(json!("한"),true).expect("buffer");assert_eq!(buffer.bytes,5);assert_eq!(buffer.claim(),vec![json!("한")]);assert_eq!(buffer.bytes,0);
        buffer.push(json!(1),true).expect("first");buffer.push(json!(2),true).expect("overflow drops instead of failing");
        assert!(buffer.overflowed);assert!(buffer.messages.is_empty());assert_eq!(buffer.bytes,0);
    }
    #[test]
    fn foreign_segments_are_dropped_until_that_turn_ends_issue_2192() {
        let mut buffer=PreReplayBuffer {messages:Vec::new(),bytes:0,max_messages:1,max_bytes:1000,overflowed:false};
        buffer.push(json!({"type":"stream_event","parent_tool_use_id":"tool"}),true).expect("subagent");assert!(buffer.messages.is_empty());
        buffer.push(json!({"type":"stream_event","parent_tool_use_id":null,"n":1}),true).expect("first");
        buffer.push(json!({"type":"stream_event","parent_tool_use_id":null,"n":2}),true).expect("overflow");
        buffer.push(json!({"type":"stream_event","parent_tool_use_id":null,"n":3}),true).expect("dropped");
        assert!(buffer.messages.is_empty());
        buffer.end_foreign_turn();assert!(!buffer.overflowed);
        buffer.push(json!({"type":"stream_event","parent_tool_use_id":null,"n":4}),true).expect("ours");assert_eq!(buffer.messages.len(),1);
        assert!(is_foreign_result(&json!({"user_message_uuid":"other"}),"u"));assert!(!is_foreign_result(&json!({"user_message_uuid":"u"}),"u"));
        assert!(is_foreign_result(&json!({"origin":{"kind":"agent"}}),"u"));assert!(!is_foreign_result(&json!({}),"u"));
    }
}
