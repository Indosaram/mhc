use std::collections::{BTreeMap,BTreeSet};
use serde_json::Value;
pub const RENDERED_COMPONENT_RECORD:&str="__senpiRenderedComponent";
struct Connection{id:String,capabilities:Vec<String>,registered_capabilities:bool,sessions:BTreeSet<String>}
#[derive(Default)]pub struct ConnectionTargets{connections:Vec<Connection>}
impl ConnectionTargets{
    pub fn register(&mut self,id:&str){if let Some(connection)=self.connections.iter_mut().find(|connection|connection.id==id){*connection=Connection{id:id.into(),capabilities:vec![],registered_capabilities:false,sessions:BTreeSet::new()};}else{self.connections.push(Connection{id:id.into(),capabilities:vec![],registered_capabilities:false,sessions:BTreeSet::new()});}}
    pub fn unregister(&mut self,id:&str){self.connections.retain(|connection|connection.id!=id);}
    pub fn attach(&mut self,id:&str,session:&str)->bool{self.connections.iter_mut().find(|connection|connection.id==id).is_some_and(|connection|connection.sessions.insert(session.into()))}
    pub fn detach(&mut self,id:&str,session:&str){if let Some(connection)=self.connections.iter_mut().find(|connection|connection.id==id){connection.sessions.remove(session);}}
    pub fn set_capabilities(&mut self,id:&str,capabilities:&[String])->bool{let Some(connection)=self.connections.iter_mut().find(|connection|connection.id==id)else{return false;};let was=connection.capabilities.iter().any(|capability|capability==crate::custom_capability::RENDERED_COMPONENTS_CAPABILITY);connection.capabilities=capabilities.iter().fold(vec![],|mut unique,capability|{if !unique.contains(capability){unique.push(capability.clone());}unique});connection.registered_capabilities=true;!was&&connection.capabilities.iter().any(|capability|capability==crate::custom_capability::RENDERED_COMPONENTS_CAPABILITY)}
    pub fn capabilities(&self,id:&str)->Option<&[String]>{self.connections.iter().find(|connection|connection.id==id&&connection.registered_capabilities).map(|connection|connection.capabilities.as_slice())}
    pub fn clear_capabilities(&mut self,id:&str){if let Some(connection)=self.connections.iter_mut().find(|connection|connection.id==id){connection.capabilities.clear();connection.registered_capabilities=false;}}
    pub fn targets(&self,session:&str,target:Option<&str>,targeted:bool,rendered:bool,record_type:Option<&str>)->Vec<Option<String>>{
        if targeted{return vec![target.map(str::to_owned)];}
        if self.connections.is_empty(){return vec![None];}
        let broadcast=record_type.is_some_and(|kind|matches!(kind,"agent_start"|"agent_settled"|"agent_idle"|"session_opened"|"session_closed"));
        self.connections.iter().filter(|connection|broadcast||(connection.sessions.contains(session)&&(!rendered||connection.capabilities.iter().any(|capability|capability==crate::custom_capability::RENDERED_COMPONENTS_CAPABILITY)))).map(|connection|Some(connection.id.clone())).collect()
    }
}
struct SnapshotRecord{line:String,placeholder_line:Option<String>,source:Option<Value>,rendered:bool}
#[derive(Default)]pub struct SessionSnapshots{snapshots:BTreeMap<String,Vec<SnapshotRecord>>,questions:BTreeMap<String,serde_json::Map<String,Value>>}
impl SessionSnapshots{
    pub fn remember(&mut self,session_id:&str,value:&Value,line:String,placeholder_line:Option<String>,source:Option<Value>){
        let kind=value["type"].as_str();let id=value["id"].as_str();
        if kind==Some("extension_ui_request")&&value["method"]=="question"&&let Some(id)=id{self.questions.entry(session_id.into()).or_default().insert(id.into(),value.clone());return;}
        if kind==Some("question_resolved")&&let Some(id)=id{if let Some(questions)=self.questions.get_mut(session_id){questions.shift_remove(id);}return;}
        if kind==Some("question_updated")&&let Some(id)=id{if let Some(frame)=self.questions.get_mut(session_id).and_then(|questions|questions.get_mut(id)){frame["deadlineAtMs"]=value["deadlineAtMs"].clone();frame["remainingMs"]=value["remainingMs"].clone();}return;}
        let record=SnapshotRecord{line,placeholder_line,source,rendered:value[RENDERED_COMPONENT_RECORD]==true};
        if kind==Some("message_start")||(kind==Some("message_update")&&value["assistantMessageEvent"]["type"]=="text_start"){self.snapshots.insert(session_id.into(),vec![record]);}else if let Some(records)=self.snapshots.get_mut(session_id){records.push(record);}
        if kind==Some("message_end"){self.snapshots.remove(session_id);}
    }
    pub fn replay(&mut self,session_id:&str,rendered:bool,placeholders:bool,now_ms:f64)->Result<Vec<String>,serde_json::Error>{
        let mut lines=vec![];
        for record in self.snapshots.get_mut(session_id).into_iter().flatten(){
            if record.rendered&&!rendered{continue;}
            if placeholders&&record.placeholder_line.is_none(){record.placeholder_line=Some(match &record.source{Some(source)=>crate::jsonl::serialize_json_line(&crate::media_placeholders::omit_inline_media(source))?,None=>record.line.clone()});}
            lines.push(if placeholders{record.placeholder_line.as_ref().unwrap_or(&record.line).clone()}else{record.line.clone()});
        }
        for frame in self.questions.get(session_id).into_iter().flat_map(serde_json::Map::values){let mut frame=frame.clone();frame["remainingMs"]=Value::from(frame["deadlineAtMs"].as_f64().map_or(0.,|deadline|(deadline-now_ms).max(0.)));lines.push(crate::jsonl::serialize_json_line(&frame)?);}
        Ok(lines)
    }
    pub fn forget(&mut self,session_id:&str){self.snapshots.remove(session_id);self.questions.remove(session_id);}
}
#[cfg(test)]mod tests{
    use super::*;use serde_json::json;
    #[test]fn pending_questions_replay_in_registration_order_after_update(){let mut snapshots=SessionSnapshots::default();for id in ["z","a"]{snapshots.remember("s",&json!({"type":"extension_ui_request","method":"question","id":id}),String::new(),None,None);}snapshots.remember("s",&json!({"type":"extension_ui_request","method":"question","id":"z","deadlineAtMs":100}),String::new(),None,None);let frames=snapshots.replay("s",false,false,0.).unwrap().into_iter().map(|line|serde_json::from_str::<Value>(&line).unwrap()["id"].as_str().unwrap().to_owned()).collect::<Vec<_>>();assert_eq!(frames,vec!["z","a"]);}
    #[test]fn targets_keep_connection_order_and_lifecycle_broadcast(){let mut targets=ConnectionTargets::default();assert_eq!(targets.targets("s",None,false,false,None),vec![None]);targets.register("z");targets.register("a");assert!(targets.attach("z","s"));assert!(!targets.attach("z","s"));assert_eq!(targets.targets("s",None,false,false,None),vec![Some("z".into())]);assert!(targets.targets("s",None,false,true,None).is_empty());assert!(targets.set_capabilities("z",&["rendered_components".into()]));assert_eq!(targets.targets("s",None,false,true,None),vec![Some("z".into())]);assert_eq!(targets.targets("s",None,false,false,Some("agent_idle")),vec![Some("z".into()),Some("a".into())]);assert_eq!(targets.targets("s",Some("unknown"),true,false,None),vec![Some("unknown".into())]);}
    #[test]fn message_end_discards_replay_and_rendered_requires_capability(){let mut snapshots=SessionSnapshots::default();let value=json!({"type":"message_start","__senpiRenderedComponent":true});snapshots.remember("s",&value,"start\n".into(),None,None);assert!(snapshots.replay("s",false,false,0.).unwrap().is_empty());assert_eq!(snapshots.replay("s",true,false,0.).unwrap(),vec!["start\n"]);snapshots.remember("s",&json!({"type":"message_end"}),"end\n".into(),None,None);assert!(snapshots.replay("s",true,false,0.).unwrap().is_empty());}
    #[test]fn question_replay_uses_updated_deadline_and_resolution_removes_it(){let mut snapshots=SessionSnapshots::default();snapshots.remember("s",&json!({"type":"extension_ui_request","method":"question","id":"q","deadlineAtMs":100}),String::new(),None,None);snapshots.remember("s",&json!({"type":"question_updated","id":"q","deadlineAtMs":200,"remainingMs":150}),String::new(),None,None);let line=snapshots.replay("s",false,false,150.).unwrap().remove(0);let value:Value=serde_json::from_str(&line).unwrap();assert_eq!(value["remainingMs"],50.);snapshots.remember("s",&json!({"type":"question_resolved","id":"q"}),String::new(),None,None);assert!(snapshots.replay("s",false,false,300.).unwrap().is_empty());}
}
