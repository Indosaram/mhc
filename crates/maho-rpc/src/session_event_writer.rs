use std::collections::{BTreeSet,VecDeque};
use serde_json::Value;
pub const MAX_SHARED_STDIO_QUEUE_BYTES:usize=64*1024*1024;
pub const MAX_SHARED_STDIO_QUEUE_RECORDS:usize=4096;
fn compact_delta(value:&Value)->Option<(&str,f64,&str)>{
    if value["type"]!="message_update"||value.get("message").is_none(){return None;}
    let event=&value["assistantMessageEvent"];let kind=event["type"].as_str()?;
    if !matches!(kind,"text_delta"|"thinking_delta"|"toolcall_delta"){return None;}
    Some((kind,event["contentIndex"].as_f64()?,event["delta"].as_str()?))
}
struct Record{value:Value,key:Option<String>}
#[derive(Default)]pub struct RecordQueue{records:VecDeque<Record>,latest:BTreeSet<String>}
impl RecordQueue{
    pub fn append(&mut self,value:Value){
        let key=if compact_delta(&value).is_some(){Some("message".to_owned())}else if value["type"]=="tool_execution_update"{value["toolCallId"].as_str().map(|id|format!("tool:{id}"))}else{None};
        if let Some(key)=&key{
            if self.latest.contains(key)&&let Some(index)=self.records.iter().position(|record|record.key.as_ref()==Some(key)){
                if key=="message"{
                    self.records[index].value["message"]=Value::Null;self.records[index].value["assistantMessageEvent"]["partial"]=Value::Null;
                    if index>0{
                        let current=compact_delta(&self.records[index].value);let previous=compact_delta(&self.records[index-1].value);
                        if let (Some((kind,content,delta)),Some((previous_kind,previous_content,previous_delta)))=(current,previous)&&self.records[index-1].value["message"].is_null()&&kind==previous_kind&&content==previous_content{
                            let merged=format!("{previous_delta}{delta}");self.records[index-1].value["assistantMessageEvent"]["delta"]=merged.into();self.records.remove(index);
                        }
                    }
                    for record in &mut self.records{if record.key.as_ref()==Some(key){record.key=None;}}
                }else{self.records.remove(index);}
            }
            self.latest.insert(key.clone());
        }else{self.latest.clear();for record in &mut self.records{record.key=None;}}
        self.records.push_back(Record{value,key});
    }
    pub fn pop(&mut self)->Option<Value>{let record=self.records.pop_front()?;if let Some(key)=record.key{self.latest.remove(&key);}Some(record.value)}
}
#[cfg(test)]mod tests{
    use super::*;use serde_json::json;
    fn delta(text:&str)->Value{json!({"type":"message_update","message":{"content":text},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":text,"partial":{}}})}
    #[test]fn compact_snapshots_merge_deltas_but_keep_latest_full(){let mut queue=RecordQueue::default();queue.append(delta("a"));queue.append(delta("b"));queue.append(delta("c"));let first=queue.pop().unwrap();assert!(first["message"].is_null());assert_eq!(first["assistantMessageEvent"]["delta"],"ab");assert_eq!(queue.pop().unwrap()["message"]["content"],"c");assert!(queue.pop().is_none());}
    #[test]fn ordering_barrier_prevents_compaction_across_response(){let mut queue=RecordQueue::default();queue.append(delta("a"));queue.append(json!({"type":"response"}));queue.append(delta("b"));assert!(!queue.pop().unwrap()["message"].is_null());assert_eq!(queue.pop().unwrap()["type"],"response");assert_eq!(queue.pop().unwrap()["assistantMessageEvent"]["delta"],"b");}
    #[test]fn tool_updates_replace_only_same_key_until_barrier(){let mut queue=RecordQueue::default();queue.append(json!({"type":"tool_execution_update","toolCallId":"t","value":1}));queue.append(json!({"type":"tool_execution_update","toolCallId":"t","value":2}));assert_eq!(queue.pop().unwrap()["value"],2);assert!(queue.pop().is_none());}
}
