use serde::{Serialize,Deserialize};
use serde_json::Value;
use std::collections::BTreeMap;
#[derive(Clone,Debug,Default,PartialEq,Eq)]
pub struct HerdrState{pub blocked:Vec<(String,Option<String>)>,pub turn_active:bool,pub child_count:u64,pub monitor_count:u64,pub wake_sources:BTreeMap<String,u64>}
pub enum HerdrStateEvent{Blocked{active:bool,id:String,label:Option<String>},Turn{active:bool},Children{count:u64},Monitors{count:u64},WakeSource{source:String,count:u64}}
/// Wake sources whose work is already reported through a dedicated field or the blocked state.
const REPRESENTED_WAKE_SOURCES:[&str;3]=["terminal-monitors","senpi-task","ask-user"];
fn wake_source_label(source:&str)->Option<(&'static str,&'static str)>{
    match source{
        "terminal-background-sessions"=>Some(("background session","background sessions")),
        "senpi-codemode"=>Some(("detached eval cell","detached eval cells")),
        "omo-dag"=>Some(("DAG run","DAG runs")),
        "loop-guard-hard-stop"=>Some(("loop-guard recovery pending","loop-guard recoveries pending")),
        _=>None,
    }
}
#[derive(Debug,Serialize,Deserialize,PartialEq,Eq)]
pub struct HerdrReport{pub state:String,#[serde(skip_serializing_if="Option::is_none")]pub message:Option<String>}
pub fn reduce_herdr_state(mut state:HerdrState,event:HerdrStateEvent)->HerdrState{
    match event{
        HerdrStateEvent::Turn{active}=>state.turn_active=active,
        HerdrStateEvent::Children{count}=>state.child_count=count,
        HerdrStateEvent::Monitors{count}=>state.monitor_count=count,
        HerdrStateEvent::WakeSource{source,count}=>{
            if state.wake_sources.get(&source).copied().unwrap_or(0)==count{return state;}
            if count>0{state.wake_sources.insert(source,count);}else{state.wake_sources.remove(&source);}
        }
        HerdrStateEvent::Blocked{active,id,label}=>{
            let index=state.blocked.iter().position(|(key,_)|*key==id);
            match (active,index){(true,None)=>state.blocked.push((id,label)),(false,Some(index))=>{state.blocked.remove(index);},_=>{}}
        }
    }
    state
}
pub fn select_herdr_report(state:&HerdrState)->HerdrReport{
    if let Some((_,message))=state.blocked.first(){return HerdrReport{state:"blocked".into(),message:message.clone()};}
    let child_count=state.child_count.max(state.wake_sources.get("senpi-task").copied().unwrap_or(0));
    let mut parts=Vec::new();if child_count>0{parts.push(format!("{} subagent{} running",child_count,if child_count==1{""}else{"s"}));}
    if state.monitor_count>0{parts.push(format!("{} monitor{} live",state.monitor_count,if state.monitor_count==1{""}else{"s"}));}
    for (source,count) in &state.wake_sources{
        if REPRESENTED_WAKE_SOURCES.contains(&source.as_str()){continue;}
        parts.push(match wake_source_label(source){Some((singular,plural))=>format!("{count} {}",if *count==1{singular}else{plural}),None=>format!("{count} {source}")});
    }
    if state.turn_active||!parts.is_empty(){HerdrReport{state:"working".into(),message:(!parts.is_empty()).then(||parts.join(" + "))}}else{HerdrReport{state:"idle".into(),message:None}}
}
pub fn is_herdr_blocked_event(value:&Value)->bool{
    value.is_object()&&value.get("active").is_some_and(Value::is_boolean)&&value.get("id").and_then(Value::as_str).is_some_and(|id|!id.is_empty())&&value.get("label").is_none_or(Value::is_string)
}
