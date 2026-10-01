use std::collections::BTreeMap;
use serde_json::Value;
pub const RENDERED_COMPONENT_RECORD:&str="__senpiRenderedComponent";
struct SnapshotRecord{line:String,placeholder_line:Option<String>,source:Option<Value>,rendered:bool}
#[derive(Default)]pub struct SessionSnapshots{snapshots:BTreeMap<String,Vec<SnapshotRecord>>,questions:BTreeMap<String,BTreeMap<String,Value>>}
impl SessionSnapshots{
    pub fn remember(&mut self,session_id:&str,value:&Value,line:String,placeholder_line:Option<String>,source:Option<Value>){
        let kind=value["type"].as_str();let id=value["id"].as_str();
        if kind==Some("extension_ui_request")&&value["method"]=="question"&&let Some(id)=id{self.questions.entry(session_id.into()).or_default().insert(id.into(),value.clone());return;}
        if kind==Some("question_resolved")&&let Some(id)=id{if let Some(questions)=self.questions.get_mut(session_id){questions.remove(id);}return;}
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
        for frame in self.questions.get(session_id).into_iter().flat_map(BTreeMap::values){let mut frame=frame.clone();frame["remainingMs"]=Value::from(frame["deadlineAtMs"].as_f64().map_or(0.,|deadline|(deadline-now_ms).max(0.)));lines.push(crate::jsonl::serialize_json_line(&frame)?);}
        Ok(lines)
    }
    pub fn forget(&mut self,session_id:&str){self.snapshots.remove(session_id);self.questions.remove(session_id);}
}
#[cfg(test)]mod tests{
    use super::*;use serde_json::json;
    #[test]fn message_end_discards_replay_and_rendered_requires_capability(){let mut snapshots=SessionSnapshots::default();let value=json!({"type":"message_start","__senpiRenderedComponent":true});snapshots.remember("s",&value,"start\n".into(),None,None);assert!(snapshots.replay("s",false,false,0.).unwrap().is_empty());assert_eq!(snapshots.replay("s",true,false,0.).unwrap(),vec!["start\n"]);snapshots.remember("s",&json!({"type":"message_end"}),"end\n".into(),None,None);assert!(snapshots.replay("s",true,false,0.).unwrap().is_empty());}
    #[test]fn question_replay_uses_updated_deadline_and_resolution_removes_it(){let mut snapshots=SessionSnapshots::default();snapshots.remember("s",&json!({"type":"extension_ui_request","method":"question","id":"q","deadlineAtMs":100}),String::new(),None,None);snapshots.remember("s",&json!({"type":"question_updated","id":"q","deadlineAtMs":200,"remainingMs":150}),String::new(),None,None);let line=snapshots.replay("s",false,false,150.).unwrap().remove(0);let value:Value=serde_json::from_str(&line).unwrap();assert_eq!(value["remainingMs"],50.);snapshots.remember("s",&json!({"type":"question_resolved","id":"q"}),String::new(),None,None);assert!(snapshots.replay("s",false,false,300.).unwrap().is_empty());}
}
