use std::{collections::BTreeMap,sync::{Arc,Mutex}};
#[derive(Debug,Clone,Default,PartialEq,Eq)]
pub struct SessionAttribution{pub session_id:Option<String>,pub tool:Option<String>}
#[derive(Default)]
struct ActivityState{sequence:u64,open:BTreeMap<u64,SessionAttribution>,last_finished:Option<(u64,SessionAttribution)>}
#[derive(Clone,Default)]
pub struct SessionActivityRegistry(Arc<Mutex<ActivityState>>);
impl SessionActivityRegistry{
    pub fn mark(&self)->u64{self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).sequence}
    pub fn since(&self,mark:u64)->Option<SessionAttribution>{let state=self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if let Some((sequence,attribution))=&state.last_finished&&*sequence>mark{return Some(attribution.clone());}state.open.last_key_value().map(|(_,attribution)|attribution.clone())}
    pub fn open_span(&self,mut attribution:SessionAttribution,ambient:Option<&SessionAttribution>)->SessionAttributionSpan{if attribution.session_id.is_none(){attribution.session_id=ambient.and_then(|ambient|ambient.session_id.clone());}let mut state=self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.sequence+=1;let mark=state.sequence;state.open.insert(mark,attribution);SessionAttributionSpan{registry:self.clone(),mark}}
}
pub struct SessionAttributionSpan{registry:SessionActivityRegistry,mark:u64}
impl SessionAttributionSpan{pub fn close(&mut self){let mut state=self.registry.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if let Some(attribution)=state.open.remove(&self.mark){state.sequence+=1;state.last_finished=Some((state.sequence,attribution));}}}
impl Drop for SessionAttributionSpan{fn drop(&mut self){self.close();}}
pub struct ToolAttributionSpans{session_id:String,registry:SessionActivityRegistry,spans:Vec<(String,SessionAttributionSpan)>}
impl ToolAttributionSpans{
    pub fn new(session_id:String,registry:SessionActivityRegistry)->Self{Self{session_id,registry,spans:vec![]}}
    pub fn close_all(&mut self){self.spans.clear();}
    pub fn observe(&mut self,record:&serde_json::Value){
        let Some(kind)=record.get("type").and_then(serde_json::Value::as_str)else{return;};
        if matches!(kind,"tool_execution_start"|"tool_execution_end")&&let Some(id)=record.get("toolCallId").and_then(serde_json::Value::as_str){
            if let Some(index)=self.spans.iter().position(|(key,_)|key==id){self.spans.remove(index);}
            if kind=="tool_execution_start"{let span=self.registry.open_span(SessionAttribution{session_id:Some(self.session_id.clone()),tool:record.get("toolName").and_then(serde_json::Value::as_str).map(str::to_owned)},None);self.spans.push((id.into(),span));}
            return;
        }
        if matches!(kind,"agent_settled"|"agent_idle"){self.close_all();}
    }
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn finished_activity_wins_over_open_and_close_is_idempotent(){let registry=SessionActivityRegistry::default();let mut first=registry.open_span(SessionAttribution{session_id:Some("first".into()),tool:None},None);let mark=registry.mark();let _second=registry.open_span(SessionAttribution{session_id:Some("second".into()),tool:None},None);first.close();assert_eq!(registry.since(mark).unwrap().session_id.as_deref(),Some("first"));let after=registry.mark();first.close();assert_eq!(registry.mark(),after);assert_eq!(registry.since(after).unwrap().session_id.as_deref(),Some("second"));}
    #[test]fn settled_turn_closes_tool_spans(){let registry=SessionActivityRegistry::default();let mut tools=ToolAttributionSpans::new("session".into(),registry.clone());tools.observe(&serde_json::json!({"type":"tool_execution_start","toolCallId":"id","toolName":"bash"}));assert_eq!(registry.since(registry.mark()).unwrap().tool.as_deref(),Some("bash"));tools.observe(&serde_json::json!({"type":"agent_settled"}));assert!(registry.since(registry.mark()).is_none());}
    #[test]fn span_inherits_only_session_id(){let registry=SessionActivityRegistry::default();let ambient=SessionAttribution{session_id:Some("ambient".into()),tool:Some("old".into())};let _span=registry.open_span(SessionAttribution::default(),Some(&ambient));assert_eq!(registry.since(0).unwrap(),SessionAttribution{session_id:ambient.session_id,tool:None});}
}
