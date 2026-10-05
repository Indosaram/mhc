use serde_json::Value;
pub fn is_replay_for(message:&Value,uuid:&str)->bool {message["type"]=="user"&&message["isReplay"]==true&&message["uuid"]==uuid}
pub fn is_autonomous_result(message:&Value)->bool {
    (message.get("origin").is_some_and(|origin|!origin.is_null()&&origin["kind"]!="human"))
        ||["parent_tool_use_id","subagent_type"].iter().any(|key|message.get(*key).is_some_and(|value|!value.is_null()))||message["isSynthetic"]==true
}
pub fn result_matches_turn(message:&Value,uuid:&str,claimed:bool)->bool {
    if let Some(value)=message.get("user_message_uuid") {return value==uuid;}
    claimed&&!is_autonomous_result(message)
}
pub struct PreReplayBuffer {pub messages:Vec<Value>,pub bytes:usize,pub max_messages:usize,pub max_bytes:usize}
impl PreReplayBuffer {
    pub fn push(&mut self,message:Value,current_generation:bool)->anyhow::Result<()> {
        self.bytes+=serde_json::to_vec(&message)?.len();self.messages.push(message);
        if self.messages.len()>self.max_messages||self.bytes>self.max_bytes {anyhow::bail!("Anthropic Subscription pre-replay buffer overflow");}
        if !current_generation {self.messages.clear();}Ok(())
    }
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
        let mut buffer=PreReplayBuffer {messages:Vec::new(),bytes:0,max_messages:1,max_bytes:100};buffer.push(json!("한"),true).expect("buffer");assert_eq!(buffer.bytes,5);assert_eq!(buffer.claim(),vec![json!("한")]);assert_eq!(buffer.bytes,0);
        buffer.push(json!(1),true).expect("first");assert!(buffer.push(json!(2),true).is_err());
    }
}
