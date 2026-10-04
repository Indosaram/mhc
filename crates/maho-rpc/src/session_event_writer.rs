use std::collections::{BTreeMap,BTreeSet,VecDeque};
use serde_json::Value;
pub const MAX_SHARED_STDIO_QUEUE_BYTES:usize=64*1024*1024;
pub const MAX_SHARED_STDIO_QUEUE_RECORDS:usize=4096;
#[derive(Default)]pub struct CloseResponseReservations{
    records:usize,
    bytes:usize,
    next_id:u64,
    pending:BTreeMap<u64,(usize,usize)>,
}
impl CloseResponseReservations{
    pub fn pending_size(&self)->(usize,usize){(self.records,self.bytes)}
    pub fn reserve(&mut self,session_id:&str,response:&Value,terminal:bool,buffered:(usize,usize))->Result<Option<u64>,serde_json::Error>{
        let mut tagged=response.clone();tagged["sessionId"]=session_id.into();
        let records=if terminal{2}else{1};
        let bytes=crate::jsonl::serialize_json_line(&tagged)?.len()+if terminal{crate::jsonl::serialize_json_line(&serde_json::json!({"type":"session_closed","sessionId":session_id,"reason":"client_close"}))?.len()}else{0};
        if buffered.0+self.records+records>MAX_SHARED_STDIO_QUEUE_RECORDS||buffered.1+self.bytes+bytes>MAX_SHARED_STDIO_QUEUE_BYTES{return Ok(None);}
        let id=self.next_id;self.next_id+=1;
        self.pending.insert(id,(records,bytes));self.records+=records;self.bytes+=bytes;Ok(Some(id))
    }
    pub fn release(&mut self,id:u64)->bool{
        let Some((records,bytes))=self.pending.remove(&id)else{return false;};
        self.records-=records;self.bytes-=bytes;true
    }
}
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
#[derive(Default)]pub struct SessionRecordScheduler{queues:BTreeMap<(Option<String>,String),RecordQueue>,ready:VecDeque<(Option<String>,String)>,in_flight:Option<(Option<String>,String)>,in_flight_bytes:usize,reservations:CloseResponseReservations,sealed:BTreeSet<String>}
pub struct SessionWriterActor{
    scheduler:std::sync::Arc<std::sync::Mutex<SessionRecordScheduler>>,
    changed:std::sync::Arc<tokio::sync::Notify>,
    state:tokio::sync::watch::Sender<Result<bool,String>>,
    task:tokio::task::JoinHandle<()>,
}
impl SessionWriterActor{
    pub fn new(mut writer:impl tokio::io::AsyncWrite+Unpin+Send+'static)->Self{
        use tokio::io::AsyncWriteExt;
        let scheduler=std::sync::Arc::new(std::sync::Mutex::new(SessionRecordScheduler::default()));
        let changed=std::sync::Arc::new(tokio::sync::Notify::new());
        let(state,_)=tokio::sync::watch::channel(Ok(true));
        let pending=scheduler.clone();let wake=changed.clone();let status=state.clone();
        let task=tokio::spawn(async move{
            loop{
                let notified=wake.notified();
                let record={
                    let mut queue=pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let record=queue.next_record();
                    if record.is_none(){let _=status.send_replace(Ok(true));}
                    record
                };
                let Some((_,record))=record else{notified.await;continue;};
                let result=async{
                    let line=crate::jsonl::serialize_json_line(&record)?;
                    writer.write_all(line.as_bytes()).await?;
                    writer.flush().await
                }.await;
                if let Err(error)=result{
                    let _queue=pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let _=status.send_replace(Err(error.to_string()));return;
                }
                pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).acknowledge_write();
            }
        });
        Self{scheduler,changed,state,task}
    }
    pub fn enqueue(&self,session_id:&str,value:Value)->Result<(),String>{
        let mut scheduler=self.scheduler.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.state.borrow().clone()?;
        let incoming=crate::jsonl::serialize_json_line(&value).map_err(|error|error.to_string())?.len();
        let (records,bytes)=scheduler.buffered_size();
        let(reserved_records,reserved_bytes)=scheduler.reservations.pending_size();
        if scheduler.sealed.contains(session_id){return Ok(());}
        if records+reserved_records>=MAX_SHARED_STDIO_QUEUE_RECORDS||bytes+reserved_bytes+incoming>MAX_SHARED_STDIO_QUEUE_BYTES{
            return Err("session_output_overflow, resync required".into());
        }
        scheduler.enqueue(session_id,None,value);
        let _=self.state.send_replace(Ok(false));self.changed.notify_one();Ok(())
    }
    pub fn reserve_close_response(&self,session_id:&str,response:&Value,terminal:bool)->Result<Option<u64>,String>{
        let mut scheduler=self.scheduler.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.state.borrow().clone()?;
        let buffered=scheduler.buffered_size();
        scheduler.reservations.reserve(session_id,response,terminal,buffered).map_err(|error|error.to_string())
    }
    pub fn release_close_response(&self,id:u64){self.scheduler.lock().unwrap_or_else(std::sync::PoisonError::into_inner).reservations.release(id);}
    pub fn forget_session(&self,session_id:&str){self.scheduler.lock().unwrap_or_else(std::sync::PoisonError::into_inner).sealed.remove(session_id);}
    pub fn complete_close_response(&self,id:u64,session_id:&str,mut response:Value,terminal:bool,reason:Option<&str>)->Result<(),String>{
        let mut scheduler=self.scheduler.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.state.borrow().clone()?;
        if !scheduler.reservations.release(id){return Ok(());}
        if terminal{
            if !scheduler.sealed.insert(session_id.into()){return Ok(());}
            let mut lifecycle=serde_json::json!({"type":"session_closed","sessionId":session_id});
            if let Some(reason)=reason{lifecycle["reason"]=reason.into();}
            scheduler.enqueue(session_id,None,lifecycle);
        }
        response["sessionId"]=session_id.into();scheduler.enqueue(session_id,None,response);
        let _=self.state.send_replace(Ok(false));self.changed.notify_one();Ok(())
    }
    pub async fn flush(&self)->Result<(),String>{
        let mut state=self.state.subscribe();
        loop{if state.borrow_and_update().clone()?{return Ok(());}state.changed().await.map_err(|error|error.to_string())?;}
    }
}
impl Drop for SessionWriterActor{fn drop(&mut self){self.task.abort();}}
impl SessionRecordScheduler{
    pub fn buffered_size(&self)->(usize,usize){
        let records=self.queues.values().flat_map(|queue|queue.records.iter());
        records.fold((usize::from(self.in_flight.is_some()),self.in_flight_bytes),|(count,bytes),record|(count+1,bytes+record.value.to_string().len()+1))
    }
    pub async fn drain(&mut self,writer:&mut(impl tokio::io::AsyncWrite+Unpin))->std::io::Result<()>{
        use tokio::io::AsyncWriteExt;
        while let Some((_,value))=self.next_record(){
            let line=crate::jsonl::serialize_json_line(&value)?;
            writer.write_all(line.as_bytes()).await?;
            writer.flush().await?;
            self.acknowledge_write();
        }
        Ok(())
    }
    pub fn enqueue(&mut self,session_id:&str,target_id:Option<&str>,value:Value){
        let key=(target_id.map(str::to_owned),session_id.to_owned());
        self.queues.entry(key.clone()).or_default().append(value);
        if self.in_flight.as_ref()!=Some(&key)&&!self.ready.contains(&key){self.ready.push_back(key);}
    }
    pub fn next_record(&mut self)->Option<(Option<String>,Value)>{
        if self.in_flight.is_some(){return None;}
        let key=self.ready.pop_front()?;let value=self.queues.get_mut(&key)?.pop()?;
        self.in_flight_bytes=value.to_string().len()+1;
        self.in_flight=Some(key.clone());Some((key.0,value))
    }
    pub fn acknowledge_write(&mut self){
        let Some(key)=self.in_flight.take()else{return;};
        self.in_flight_bytes=0;
        if self.queues.get(&key).is_some_and(|queue|!queue.records.is_empty()){self.ready.push_back(key);}else{self.queues.remove(&key);}
    }
}
#[cfg(test)]mod tests{
    use super::*;use serde_json::json;
    fn delta(text:&str)->Value{json!({"type":"message_update","message":{"content":text},"assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":text,"partial":{}}})}
    #[test]fn compact_snapshots_merge_deltas_but_keep_latest_full(){let mut queue=RecordQueue::default();queue.append(delta("a"));queue.append(delta("b"));queue.append(delta("c"));let first=queue.pop().unwrap();assert!(first["message"].is_null());assert_eq!(first["assistantMessageEvent"]["delta"],"ab");assert_eq!(queue.pop().unwrap()["message"]["content"],"c");assert!(queue.pop().is_none());}
    #[test]fn ordering_barrier_prevents_compaction_across_response(){let mut queue=RecordQueue::default();queue.append(delta("a"));queue.append(json!({"type":"response"}));queue.append(delta("b"));assert!(!queue.pop().unwrap()["message"].is_null());assert_eq!(queue.pop().unwrap()["type"],"response");assert_eq!(queue.pop().unwrap()["assistantMessageEvent"]["delta"],"b");}
    #[test]fn incomplete_delta_is_an_ordering_barrier_for_snapshots_and_tool_updates(){
        let mut queue=RecordQueue::default();
        let mut incomplete=delta("incomplete");incomplete["assistantMessageEvent"].as_object_mut().unwrap().remove("contentIndex");
        let before=delta("before");let after=delta("after");
        let tool_before=json!({"type":"tool_execution_update","toolCallId":"t","value":1});
        let tool_after=json!({"type":"tool_execution_update","toolCallId":"t","value":2});
        let expected=[before,tool_before,incomplete,tool_after,after];
        for record in &expected{queue.append(record.clone());}
        for record in expected{assert_eq!(queue.pop(),Some(record));}
        assert!(queue.pop().is_none());
    }
    #[test]fn tool_updates_replace_only_same_key_until_barrier(){let mut queue=RecordQueue::default();queue.append(json!({"type":"tool_execution_update","toolCallId":"t","value":1}));queue.append(json!({"type":"tool_execution_update","toolCallId":"t","value":2}));assert_eq!(queue.pop().unwrap()["value"],2);assert!(queue.pop().is_none());}
    #[test]fn scheduler_round_robins_only_after_current_record_drains(){let mut scheduler=SessionRecordScheduler::default();scheduler.enqueue("a",None,json!({"n":1}));scheduler.enqueue("a",None,json!({"n":2}));scheduler.enqueue("b",None,json!({"n":3}));assert_eq!(scheduler.next_record().unwrap().1["n"],1);assert!(scheduler.next_record().is_none());scheduler.enqueue("a",None,json!({"n":4}));scheduler.acknowledge_write();assert_eq!(scheduler.next_record().unwrap().1["n"],3);scheduler.acknowledge_write();assert_eq!(scheduler.next_record().unwrap().1["n"],2);scheduler.acknowledge_write();assert_eq!(scheduler.next_record().unwrap().1["n"],4);scheduler.acknowledge_write();assert!(scheduler.next_record().is_none());}
}
