use serde::{Serialize,Deserialize};
use serde_json::Value;
#[derive(Clone,Debug,Default,PartialEq,Eq)]
pub struct HerdrState{pub blocked:Vec<(String,Option<String>)>,pub turn_active:bool,pub child_count:u64,pub monitor_count:u64}
pub enum HerdrStateEvent{Blocked{active:bool,id:String,label:Option<String>},Turn{active:bool},Children{count:u64},Monitors{count:u64}}
#[derive(Debug,Serialize,Deserialize,PartialEq,Eq)]
pub struct HerdrReport{pub state:String,#[serde(skip_serializing_if="Option::is_none")]pub message:Option<String>}
pub fn reduce_herdr_state(mut state:HerdrState,event:HerdrStateEvent)->HerdrState{
    match event{
        HerdrStateEvent::Turn{active}=>state.turn_active=active,
        HerdrStateEvent::Children{count}=>state.child_count=count,
        HerdrStateEvent::Monitors{count}=>state.monitor_count=count,
        HerdrStateEvent::Blocked{active,id,label}=>{
            let index=state.blocked.iter().position(|(key,_)|*key==id);
            match (active,index){(true,None)=>state.blocked.push((id,label)),(false,Some(index))=>{state.blocked.remove(index);},_=>{}}
        }
    }
    state
}
pub fn select_herdr_report(state:&HerdrState)->HerdrReport{
    if let Some((_,message))=state.blocked.first(){return HerdrReport{state:"blocked".into(),message:message.clone()};}
    let mut parts=Vec::new();if state.child_count>0{parts.push(format!("{} subagent{} running",state.child_count,if state.child_count==1{""}else{"s"}));}
    if state.monitor_count>0{parts.push(format!("{} monitor{} live",state.monitor_count,if state.monitor_count==1{""}else{"s"}));}
    if state.turn_active||!parts.is_empty(){HerdrReport{state:"working".into(),message:(!parts.is_empty()).then(||parts.join(" + "))}}else{HerdrReport{state:"idle".into(),message:None}}
}
pub fn is_herdr_blocked_event(value:&Value)->bool{
    value.is_object()&&value.get("active").is_some_and(Value::is_boolean)&&value.get("id").and_then(Value::as_str).is_some_and(|id|!id.is_empty())&&value.get("label").is_none_or(Value::is_string)
}
