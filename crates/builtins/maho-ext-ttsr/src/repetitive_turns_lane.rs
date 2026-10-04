use std::collections::BTreeSet;
use serde_json::Value;
use crate::detectors::repetitive_turns::{create_repetitive_turns_state, is_near_duplicate_of_previous_turn, normalize_turn_text, record_turn_text, RepetitiveTurnsState, REPETITIVE_TURNS_MIN_NORMALIZED_CHARS, REPETITIVE_TURNS_RULE_NAME};
pub fn collect_assistant_text(content: &Value) -> Option<String> { let text=content.as_array()?.iter().filter(|block| block.get("type").and_then(Value::as_str)==Some("text")).filter_map(|block| block.get("text").and_then(Value::as_str)).collect::<String>(); (!text.is_empty()).then_some(text) }
pub fn read_persisted_assistant_texts(entries: &[Value]) -> Vec<String> { let mut texts: Vec<_>=entries.iter().filter(|entry| entry.get("type").and_then(Value::as_str)==Some("message")).filter_map(|entry| entry.get("message")).filter(|message| message.get("role").and_then(Value::as_str)==Some("assistant")).filter_map(|message| message.get("content").and_then(collect_assistant_text)).collect(); if texts.len()>8 { texts.drain(..texts.len()-8); } texts }
pub struct RepetitiveTurnsLane { state: RepetitiveTurnsState, armed: bool, recovering: bool, current_turn_text: String, last_completed_turn_text: Option<String>, enabled: bool }
impl Default for RepetitiveTurnsLane { fn default() -> Self { Self { state: create_repetitive_turns_state(), armed:false, recovering:false, current_turn_text:String::new(), last_completed_turn_text:None, enabled:true } } }
impl RepetitiveTurnsLane {
    pub fn configure(&mut self, disabled_rules: &BTreeSet<String>) { self.enabled = !disabled_rules.contains(REPETITIVE_TURNS_RULE_NAME); }
    pub fn reset_session(&mut self) { self.state=create_repetitive_turns_state(); self.armed=false; self.recovering=false; self.current_turn_text.clear(); self.last_completed_turn_text=None; }
    pub fn restore_from_history(&mut self, texts: &[String]) { for text in texts { self.record_completed_turn(text); } self.armed=false; }
    pub fn reset_turn(&mut self) { self.armed=false; self.current_turn_text.clear(); }
    pub fn disarm(&mut self) { self.armed=false; }
    pub const fn armed(&self) -> bool { self.armed }
    pub fn observe_text_delta(&mut self, delta: &str, can_arm: bool) -> bool {
        if self.enabled && !self.armed && !self.recovering && can_arm && let Some(previous)=&self.last_completed_turn_text {
            let candidate=normalize_turn_text(&format!("{}{delta}",self.current_turn_text));
            if candidate.encode_utf16().count()>=REPETITIVE_TURNS_MIN_NORMALIZED_CHARS && is_near_duplicate_of_previous_turn(&candidate,previous) { self.armed=true; }
        }
        self.current_turn_text.push_str(delta); self.armed
    }
    pub fn commit_armed_turn(&mut self, text: Option<&str>) { self.armed=false; self.recovering=true; if let Some(text)=text { self.last_completed_turn_text=Some(normalize_turn_text(text)); } }
    pub fn record_completed_turn(&mut self, text: &str) {
        self.last_completed_turn_text=Some(normalize_turn_text(text));
        if !self.enabled { return; }
        if record_turn_text(&mut self.state,text).is_none() { self.recovering=false; } else { self.armed=true; }
    }
}
#[cfg(test)] mod tests {
    use super::*;
    const TEXT: &str="The current matrix is progressing through the native integration checks while the remaining jobs continue running.";
    #[test] fn previous_turn_arms_matching_stream() { let mut lane=RepetitiveTurnsLane::default(); lane.record_completed_turn(TEXT); lane.reset_turn(); let result=lane.observe_text_delta(TEXT,true); assert!(result); }
    #[test] fn disabled_rule_does_not_arm() { let mut lane=RepetitiveTurnsLane::default(); lane.configure(&BTreeSet::from([REPETITIVE_TURNS_RULE_NAME.into()])); lane.record_completed_turn(TEXT); let result=lane.observe_text_delta(TEXT,true); assert!(!result); }
    #[test] fn recovery_suppresses_stream_arm() { let mut lane=RepetitiveTurnsLane::default(); lane.record_completed_turn(TEXT); lane.commit_armed_turn(Some(TEXT)); let result=lane.observe_text_delta(TEXT,true); assert!(!result); }
    #[test] fn restored_history_does_not_leave_lane_armed() { let mut lane=RepetitiveTurnsLane::default(); lane.restore_from_history(&vec![TEXT.into();3]); let result=lane.armed(); assert!(!result); }
    #[test] fn permission_to_arm_is_required() { let mut lane=RepetitiveTurnsLane::default(); lane.record_completed_turn(TEXT); let result=lane.observe_text_delta(TEXT,false); assert!(!result); }
    #[test] fn reset_session_forgets_prior_turn() { let mut lane=RepetitiveTurnsLane::default(); lane.record_completed_turn(TEXT); lane.reset_session(); let result=lane.observe_text_delta(TEXT,true); assert!(!result); }
    #[test] fn persisted_history_retains_last_eight_assistant_texts() { let entries=(0..10).map(|i| serde_json::json!({"type":"message","message":{"role":"assistant","content":[{"type":"text","text":i.to_string()}]}})).collect::<Vec<_>>(); let result=read_persisted_assistant_texts(&entries); assert_eq!(result.len(),8); assert_eq!(result[0],"2"); assert_eq!(result[7],"9"); }
    #[test] fn text_collection_concatenates_without_separator() { let content=serde_json::json!([{"type":"text","text":"one"},{"type":"thinking","thinking":"ignore"},{"type":"text","text":"two"}]); let result=collect_assistant_text(&content); assert_eq!(result.as_deref(),Some("onetwo")); }
}
