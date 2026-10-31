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
struct Record{value:Value,key:Option<String>,bytes:usize}
/// Serialized JSONL size of one record, kept per record so the buffered total is O(1).
fn line_len(value:&Value)->usize{value.to_string().len()+1}
#[derive(Default)]pub struct RecordQueue{records:VecDeque<Record>,latest:BTreeSet<String>,bytes:usize}
impl RecordQueue{
    fn resize(&mut self,index:usize){let bytes=line_len(&self.records[index].value);self.bytes=self.bytes-self.records[index].bytes+bytes;self.records[index].bytes=bytes;}
    fn remove_at(&mut self,index:usize){if let Some(record)=self.records.remove(index){self.bytes-=record.bytes;}}
    pub fn append(&mut self,value:Value){let bytes=line_len(&value);self.append_with_len(value,bytes);}
    /// `append` with the record's already-measured JSONL size (`bytes == line_len(&value)`).
    fn append_with_len(&mut self,value:Value,bytes:usize){
        let key=if compact_delta(&value).is_some(){Some("message".to_owned())}else if value["type"]=="tool_execution_update"{value["toolCallId"].as_str().map(|id|format!("tool:{id}"))}else{None};
        if let Some(key)=&key{
            if self.latest.contains(key)&&let Some(index)=self.records.iter().position(|record|record.key.as_ref()==Some(key)){
                if key=="message"{
                    self.records[index].value["message"]=Value::Null;self.records[index].value["assistantMessageEvent"]["partial"]=Value::Null;
                    let mut merged_away=false;
                    if index>0{
                        let current=compact_delta(&self.records[index].value);let previous=compact_delta(&self.records[index-1].value);
                        if let (Some((kind,content,delta)),Some((previous_kind,previous_content,previous_delta)))=(current,previous)&&self.records[index-1].value["message"].is_null()&&kind==previous_kind&&content==previous_content{
                            let merged=format!("{previous_delta}{delta}");self.records[index-1].value["assistantMessageEvent"]["delta"]=merged.into();self.resize(index-1);self.remove_at(index);merged_away=true;
                        }
                    }
                    if !merged_away{self.resize(index);}
                    for record in &mut self.records{if record.key.as_ref()==Some(key){record.key=None;}}
                }else{self.remove_at(index);}
            }
            self.latest.insert(key.clone());
        }else{self.latest.clear();for record in &mut self.records{record.key=None;}}
        self.bytes+=bytes;
        self.records.push_back(Record{value,key,bytes});
    }
    pub fn pop(&mut self)->Option<Value>{self.pop_sized().map(|(value,_)|value)}
    fn pop_sized(&mut self)->Option<(Value,usize)>{let record=self.records.pop_front()?;self.bytes-=record.bytes;if let Some(key)=record.key{self.latest.remove(&key);}Some((record.value,record.bytes))}
}
#[derive(Default)]pub struct SessionRecordScheduler{queues:BTreeMap<(Option<String>,String),RecordQueue>,ready:VecDeque<(Option<String>,String)>,in_flight:Option<(Option<String>,String)>,in_flight_bytes:usize,reservations:CloseResponseReservations,sealed:BTreeSet<String>}
pub struct SessionWriterActor{
    /// Receivers of the internal structured lane (senpi `structuredLanes`); never a legacy destination.
    pub structured_lanes:crate::session_structured_lanes::SessionStructuredLanes,
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
        Self{structured_lanes:Default::default(),scheduler,changed,state,task}
    }
    pub fn enqueue(&self,session_id:&str,mut value:Value)->Result<(),String>{
        // Lifecycle copies are stamped where the host enqueues them (senpi `observeLifecycleDelivery`).
        crate::lifecycle_delivery::observe_lifecycle_delivery(&mut value);
        let mut scheduler=self.scheduler.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.state.borrow().clone()?;
        let incoming=crate::jsonl::serialize_json_line(&value).map_err(|error|error.to_string())?.len();
        let (records,bytes)=scheduler.buffered_size();
        let(reserved_records,reserved_bytes)=scheduler.reservations.pending_size();
        if scheduler.sealed.contains(session_id){return Ok(());}
        if records+reserved_records>=MAX_SHARED_STDIO_QUEUE_RECORDS||bytes+reserved_bytes+incoming>MAX_SHARED_STDIO_QUEUE_BYTES{
            return Err("session_output_overflow, resync required".into());
        }
        scheduler.enqueue_sized(session_id,value,incoming);
        let _=self.state.send_replace(Ok(false));self.changed.notify_one();Ok(())
    }
    pub fn reserve_close_response(&self,session_id:&str,response:&Value,terminal:bool)->Result<Option<u64>,String>{
        let mut scheduler=self.scheduler.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.state.borrow().clone()?;
        let buffered=scheduler.buffered_size();
        scheduler.reservations.reserve(session_id,response,terminal,buffered).map_err(|error|error.to_string())
    }
    pub fn release_close_response(&self,id:u64){self.scheduler.lock().unwrap_or_else(std::sync::PoisonError::into_inner).reservations.release(id);}
    /// senpi `isSessionSealed`: the writer accepts no further output for this session.
    pub fn is_session_sealed(&self,session_id:&str)->bool{self.scheduler.lock().unwrap_or_else(std::sync::PoisonError::into_inner).sealed.contains(session_id)}
    /// senpi `registerAcceptance`: a sealed session takes no new lane.
    pub fn register_acceptance(&self,fanout:&std::sync::Mutex<crate::session_event_fanout::SessionEventFanout>,session_id:&str,lane:crate::session_registry::SharedAcceptanceLane){
        if self.is_session_sealed(session_id){return;}
        let lanes=std::sync::Arc::clone(&fanout.lock().unwrap_or_else(std::sync::PoisonError::into_inner).acceptance);
        lanes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(session_id.to_owned(),lane);
    }
    pub fn forget_session(&self,session_id:&str){self.scheduler.lock().unwrap_or_else(std::sync::PoisonError::into_inner).sealed.remove(session_id);}
    pub fn complete_close_response(&self,id:u64,session_id:&str,mut response:Value,terminal:bool,reason:Option<&str>)->Result<(),String>{
        let mut scheduler=self.scheduler.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.state.borrow().clone()?;
        if !scheduler.reservations.release(id){return Ok(());}
        // A terminal close racing an overflow/park seal does not re-seal: only the closed response is appended.
        if terminal&&scheduler.sealed.insert(session_id.into()){
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
        self.queues.values().fold((usize::from(self.in_flight.is_some()),self.in_flight_bytes),|(count,bytes),queue|(count+queue.records.len(),bytes+queue.bytes))
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
    /// Untargeted `enqueue` reusing the size the caller already serialized for its overflow check.
    fn enqueue_sized(&mut self,session_id:&str,value:Value,bytes:usize){
        let key=(None,session_id.to_owned());
        self.queues.entry(key.clone()).or_default().append_with_len(value,bytes);
        if self.in_flight.as_ref()!=Some(&key)&&!self.ready.contains(&key){self.ready.push_back(key);}
    }
    pub fn next_record(&mut self)->Option<(Option<String>,Value)>{
        if self.in_flight.is_some(){return None;}
        let key=self.ready.pop_front()?;let (value,bytes)=self.queues.get_mut(&key)?.pop_sized()?;
        self.in_flight_bytes=bytes;
        self.in_flight=Some(key.clone());Some((key.0,value))
    }
    pub fn acknowledge_write(&mut self){
        let Some(key)=self.in_flight.take()else{return;};
        self.in_flight_bytes=0;
        if self.queues.get(&key).is_some_and(|queue|!queue.records.is_empty()){self.ready.push_back(key);}else{self.queues.remove(&key);}
    }
}
/// The writer's MCP presenter integration (senpi `SessionEventWriter` presenter state): one authority when the
/// deployment enables presenters, a presenter connection per registered RPC connection, and one private form
/// binding per session. Private records bypass event queues, snapshots, mirrors, replay and overflow previews.
#[derive(Default)]
pub struct SessionMcpPresenters {
    authority: std::sync::OnceLock<crate::mcp_presenter_authority::McpPresenterAuthority>,
    connections: std::sync::Mutex<BTreeMap<String, crate::mcp_presenter_authority::McpPresenterConnection>>,
    bindings: std::sync::Mutex<BTreeMap<String, std::sync::Arc<crate::mcp_form_types::McpFormBinding>>>,
}
/// Who answers the connection's raw writes: `false` when the connection is gone or its write failed.
pub type PresenterSend = std::sync::Arc<dyn Fn(&Value) -> bool + Send + Sync>;
fn guard<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl SessionMcpPresenters {
    /// Deployment switch. Ordinary RPC behavior is unchanged unless the host explicitly enables it.
    pub fn enable(&self) -> crate::mcp_presenter_authority::McpPresenterAuthority {
        self.authority.get_or_init(Default::default).clone()
    }
    /// senpi `mcpPresenters`.
    pub fn authority(&self) -> Option<&crate::mcp_presenter_authority::McpPresenterAuthority> {
        self.authority.get()
    }
    pub fn enabled(&self) -> bool {
        self.authority.get().is_some()
    }
    pub fn connection(&self, id: &str) -> Option<crate::mcp_presenter_authority::McpPresenterConnection> {
        guard(&self.connections).get(id).cloned()
    }
    pub fn find_connection(&self, nonce: &str) -> Option<crate::mcp_presenter_authority::McpPresenterConnection> {
        guard(&self.connections).values().find(|connection| connection.nonce == nonce).cloned()
    }
    pub fn binding(&self, session_id: &str) -> Option<std::sync::Arc<crate::mcp_form_types::McpFormBinding>> {
        guard(&self.bindings).get(session_id).cloned()
    }
    /// senpi `bindMcpForms`: binds the session, attaches every connection already attached to it, and registers the
    /// private forms for the durable session. `None` when presenters are off.
    pub fn bind(
        &self,
        session_id: &str,
        durable_id: &str,
        admit: std::sync::Arc<dyn Fn() + Send + Sync>,
        attached: &[String],
    ) -> Option<McpFormsUnbind> {
        let authority = self.authority.get()?;
        let binding = authority.bind(session_id, durable_id, admit);
        guard(&self.bindings).insert(session_id.to_owned(), std::sync::Arc::clone(&binding));
        for id in attached {
            if let Some(connection) = self.connection(id) {
                authority.attach(&connection, &binding);
            }
        }
        let registration = crate::mcp_form_types::bind_private_mcp_forms(durable_id, std::sync::Arc::clone(&binding));
        Some(McpFormsUnbind{session_id:session_id.to_owned(),binding,_registration:registration})
    }
    /// The per-connection presenter (senpi `registerConnection` presenter half); a previous one is detached first.
    pub fn register_connection(&self, id: &str, send: PresenterSend) {
        if let Some(previous) = guard(&self.connections).remove(id)
            && let Some(authority) = self.authority.get()
        {
            authority.detach(&previous, None);
        }
        if let Some(authority) = self.authority.get() {
            let connection = authority.connect(id, send);
            guard(&self.connections).insert(id.to_owned(), connection);
        }
    }
    pub fn unregister_connection(&self, id: &str) {
        let connection = guard(&self.connections).remove(id);
        if let (Some(connection), Some(authority)) = (connection, self.authority.get()) {
            authority.detach(&connection, None);
        }
    }
    pub fn attach(&self, id: &str, session_id: &str) {
        if let (Some(authority), Some(connection), Some(binding)) =
            (self.authority.get(), self.connection(id), self.binding(session_id))
        {
            authority.attach(&connection, &binding);
        }
    }
    pub fn detach(&self, id: &str, session_id: &str) {
        if let (Some(authority), Some(connection), Some(binding)) =
            (self.authority.get(), self.connection(id), self.binding(session_id))
        {
            authority.detach(&connection, Some(binding.id));
        }
    }
    /// A connection whose capabilities lack `interactive_user` + `mcp_form_presenter_v1` loses its grants.
    pub fn capabilities_changed(&self, id: &str, capabilities: &[String]) {
        let presents = capabilities.iter().any(|value| value == "interactive_user")
            && capabilities.iter().any(|value| value == crate::host_capabilities::MCP_FORM_PRESENTER_CAPABILITY);
        if !presents
            && let (Some(authority), Some(connection)) = (self.authority.get(), self.connection(id))
        {
            authority.withdraw(&connection);
        }
    }
    /// A sealed or closed session's binding is disposed (senpi `closeSession` / `sealLifecycle`).
    pub fn dispose_session(&self, session_id: &str) {
        let binding = guard(&self.bindings).remove(session_id);
        if let Some(binding) = binding {
            binding.dispose();
        }
    }
    /// senpi `handleMcpForm`: `None` when presenters are off or the record is not private; otherwise the
    /// `mcp_form_reply` to write raw to the sending connection (outside queues, snapshots and replay).
    pub fn handle(&self, value: &Value, connection: Option<&str>) -> Option<Value> {
        let authority = self.authority.get()?;
        let kind = value.get("type").and_then(Value::as_str)?;
        if !kind.starts_with("mcp_form_") && kind != "mcp_presenter_context" {
            return None;
        }
        let sender = connection.and_then(|id| self.connection(id));
        let reply = if kind == "mcp_presenter_context" {
            let context = match (value.get("sessionId").and_then(Value::as_str), &sender) {
                (Some(session_id), Some(sender)) => authority.connection_context(sender, session_id),
                _ => None,
            };
            match context {
                Some(context) => serde_json::json!({"ok":true,"context":context.to_value()}),
                None => serde_json::json!({"error":crate::mcp_presenter_authority::PRESENTER_NOT_AUTHORIZED}),
            }
        } else {
            match &sender {
                Some(sender) => authority.receive(sender, value).to_value(),
                None => serde_json::json!({"error":crate::mcp_presenter_authority::PRESENTER_NOT_AUTHORIZED}),
            }
        };
        let mut answer = serde_json::json!({"type":"mcp_form_reply"});
        if let Some(id) = value.get("id").filter(|id| id.is_string()) {
            answer["id"] = id.clone();
        }
        if let (Some(answer), Some(reply)) = (answer.as_object_mut(), reply.as_object()) {
            answer.extend(reply.clone());
        }
        Some(answer)
    }
    /// A raw reply write failed: the sender loses its presenter state (senpi `catch { detach(sender) }`).
    pub fn reply_failed(&self, id: &str) {
        if let (Some(authority), Some(connection)) = (self.authority.get(), self.connection(id)) {
            authority.detach(&connection, None);
        }
    }
}
/// What `bindMcpForms` returns: disposing the binding and forgetting it unless a newer one replaced it.
pub struct McpFormsUnbind{session_id:String,binding:std::sync::Arc<crate::mcp_form_types::McpFormBinding>,_registration:crate::mcp_form_types::PrivateFormsRegistration}
impl McpFormsUnbind{
    pub fn binding(&self)->&std::sync::Arc<crate::mcp_form_types::McpFormBinding>{&self.binding}
    pub fn unbind(self,presenters:&SessionMcpPresenters){
        self.binding.dispose();
        let mut bindings=guard(&presenters.bindings);
        if bindings.get(&self.session_id).is_some_and(|current|current.id==self.binding.id){bindings.remove(&self.session_id);}
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
    /// 3913-sealed-reattach / 3913-writer-accounting "delivers an admitted close response even when overflow already
    /// sealed the handle": a terminal close on a sealed session appends only its response, never a second seal.
    #[tokio::test]async fn an_admitted_close_on_a_sealed_session_appends_only_its_response(){
        use tokio::io::AsyncReadExt;
        let(writer,mut reader)=tokio::io::duplex(64*1024);
        let actor=SessionWriterActor::new(writer);
        let first=json!({"type":"response","command":"close_session","success":false});
        let id=actor.reserve_close_response("session",&first,true).unwrap().unwrap();
        actor.complete_close_response(id,"session",first.clone(),true,Some("client_close")).unwrap();
        assert!(actor.is_session_sealed("session"));
        let reply=json!({"id":"close","type":"response","command":"close_session","success":true});
        let id=actor.reserve_close_response("session",&reply,true).unwrap().unwrap();
        actor.complete_close_response(id,"session",reply,true,Some("client_close")).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2),actor.flush()).await.unwrap().unwrap();
        drop(actor);
        let mut wire=String::new();reader.read_to_string(&mut wire).await.unwrap();
        let records=wire.lines().map(|line|serde_json::from_str::<Value>(line).unwrap()).collect::<Vec<_>>();
        assert_eq!(records.iter().filter(|record|record["type"]=="session_closed").count(),1);
        assert_eq!(records.last(),Some(&json!({"sessionId":"session","id":"close","type":"response","command":"close_session","success":true})));
    }
    #[test]fn presenters_answer_private_records_only_when_enabled_and_withdraw_on_capability_loss(){
        let presenters=SessionMcpPresenters::default();
        assert_eq!(presenters.handle(&json!({"type":"mcp_form_sync","id":"1"}),Some("c")),None);
        presenters.enable();
        presenters.register_connection("c",std::sync::Arc::new(|_|true));
        let unbind=presenters.bind("s","durable-writer-test",std::sync::Arc::new(||{}),&["c".to_owned()]).unwrap();
        let context=presenters.handle(&json!({"type":"mcp_presenter_context","id":"2","sessionId":"s"}),Some("c")).unwrap();
        assert_eq!(context["type"],"mcp_form_reply");assert_eq!(context["id"],"2");assert_eq!(context["ok"],true);
        assert_eq!(context["context"]["bindingEpoch"],unbind.binding().epoch.as_str());
        assert_eq!(presenters.handle(&json!({"type":"mcp_form_sync","id":"3","sessionId":"s"}),Some("c")).unwrap()["error"],"presenter_not_authorized");
        assert_eq!(presenters.handle(&json!({"type":"get_state"}),Some("c")),None);
        let authority=presenters.authority().unwrap().clone();
        let owner=authority.enroll(None);
        let connection=presenters.connection("c").unwrap();
        let registration=owner.register(unbind.binding(),&connection,crate::mcp_form_types::McpPresenterKind::Desktop).unwrap();
        owner.acquire(&registration).unwrap();
        assert!(unbind.binding().capture().is_some());
        presenters.capabilities_changed("c",&["interactive_user".to_owned()]);
        assert!(unbind.binding().capture().is_none());
        assert!(crate::mcp_form_types::private_mcp_forms("durable-writer-test").is_some());
        unbind.unbind(&presenters);
        assert!(presenters.binding("s").is_none());
        assert!(crate::mcp_form_types::private_mcp_forms("durable-writer-test").is_none());
    }
}
