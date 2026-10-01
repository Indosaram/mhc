use serde_json::{Value,json};
pub const STOP_STATE_CUSTOM_TYPE:&str="senpi.hooks.stop-state";
pub const STOP_DIAGNOSTICS_CUSTOM_TYPE:&str="senpi.hooks.stop-diagnostics";
pub const STOP_OUTPUT_CUSTOM_TYPE:&str="senpi.hooks.stop-output";
pub const STOP_REENTRY_LIMIT:usize=8;
#[derive(Default)]
pub struct StopTurnTracker {active_turn_key:Option<String>,turn_index:usize}
impl StopTurnTracker {
    pub fn reset(&mut self) {self.turn_index+=1;self.active_turn_key=None;}
    pub fn turn_key(&mut self,leaf_id:Option<&str>,session_id:&str)->String {self.active_turn_key.get_or_insert_with(||format!("{}:{}",self.turn_index,leaf_id.unwrap_or(session_id))).clone()}
}
pub fn build_stop_hook_input(messages:&[Value],cwd:&str,session_id:&str,transcript_path:Option<&str>)->Value {
    let mut input=json!({"cwd":cwd,"event":"Stop","hook_event_name":"Stop","session_id":session_id});
    if let Some(reason)=messages.iter().rev().find(|message|message.get("role").and_then(Value::as_str)==Some("assistant")).and_then(|message|message.get("stopReason")) {input["stopReason"]=reason.clone();}
    if let Some(path)=transcript_path {input["transcript_path"]=json!(path);}
    input
}
#[cfg(test)]
mod tests {use super::*;#[test] fn turn_key_is_stable_until_reset() {let mut tracker=StopTurnTracker::default();assert_eq!(tracker.turn_key(Some("leaf"),"s"),"0:leaf");assert_eq!(tracker.turn_key(Some("next"),"s"),"0:leaf");tracker.reset();assert_eq!(tracker.turn_key(None,"s"),"1:s");}#[test] fn last_assistant_only() {let input=build_stop_hook_input(&[json!({"role":"assistant","stopReason":"stop"}),json!({"role":"user","stopReason":"ignored"})],"/repo","s",None);assert_eq!(input["stopReason"],"stop");assert!(input.get("transcript_path").is_none());}}
