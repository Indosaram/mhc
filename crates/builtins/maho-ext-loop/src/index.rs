use crate::{tick_prompt::{LoopMode,TickDelivery},types::{EpochMs,LoopEndReason,LoopId,LoopSentinel,DeliveryId}};
use serde::Serialize;
pub const LOOP_TICK_ENTRY_TYPE:&str="loop-tick";
pub struct PreparedLoopTick { pub text:String,pub entry:LoopTickEntryData,pub delivery_state:crate::types::SentinelDeliveryState,pub defer:bool }
pub fn prepare_loop_tick(entry:&crate::types::CronEntry,tick:&crate::scheduler::LoopTick,delivery_state:crate::types::SentinelDeliveryState,loop_file:crate::tick_prompt::LoopFileSnapshot,busy:bool,commands:&[maho_ext_api::SlashCommandInfo])->Option<PreparedLoopTick> {
    let (fields,lifecycle,mode)=match entry {
        crate::types::CronEntry::Fixed { fields,lifecycle,.. }=>(fields,lifecycle,crate::types::LoopKind::Fixed),
        crate::types::CronEntry::Dynamic { fields,lifecycle,.. }=>(fields,lifecycle,crate::types::LoopKind::Dynamic),
    };
    if lifecycle.phase==crate::types::LoopPhase::Ended { return None; }
    let built=crate::tick_prompt::build_tick_message(crate::tick_prompt::TickPromptInput { loop_id:fields.id.clone(),delivery_id:tick.delivery_id.clone(),mode,payload:fields.payload.clone(),reentry_prompt:fields.reentry_prompt.clone(),delivery_state,loop_file });
    let command=built.text.strip_prefix('/').and_then(|text|text.split(char::is_whitespace).next());
    let defer=busy&&command.is_some_and(|name|commands.iter().any(|command|command.name==name));
    Some(PreparedLoopTick { text:built.text,entry:LoopTickEntryData { loop_id:fields.id.clone(),delivery_id:tick.delivery_id.clone(),scheduled_for_at:tick.scheduled_for_at,mode,delivery:built.delivery,sentinel:built.details.sentinel,noop_streak:fields.noop_streak,folded:fields.noop_streak>=2.0 },delivery_state:built.delivery_state,defer })
}
pub fn deliver_loop_tick(api:&maho_ext_api::ExtensionApi,prepared:PreparedLoopTick,busy:bool)->Result<(),maho_ext_api::ExtensionFailure> {
    if prepared.defer { return Err(maho_ext_api::ExtensionFailure::new("Loop extension command must be deferred until settlement")); }
    api.append_entry(LOOP_TICK_ENTRY_TYPE,Some(serde_json::to_value(prepared.entry).map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?))?;
    api.send_user_message(maho_ext_api::UserMessageContent::Text(prepared.text),maho_ext_api::SendUserMessageOptions { expand_prompt_templates:true,deliver_as:busy.then_some(maho_ext_api::StreamingBehavior::FollowUp) })
}
#[derive(Clone,Debug,PartialEq,Serialize)]
#[serde(rename_all="camelCase")]
pub struct LoopTickEntryData {
    pub loop_id:LoopId,pub delivery_id:DeliveryId,pub scheduled_for_at:EpochMs,
    pub mode:LoopMode,pub delivery:TickDelivery,
    #[serde(skip_serializing_if="Option::is_none")] pub sentinel:Option<LoopSentinel>,
    pub noop_streak:f64,pub folded:bool,
}
pub struct LoopCreateOk {
    pub loop_id:LoopId,pub superseded_loop_id:Option<LoopId>,pub rounding_notice:Option<String>,
    pub cron_expression:Option<String>,pub effective_cadence:Option<String>,pub expires_at:Option<EpochMs>,
}
pub enum LoopCreateOutcome { Created(LoopCreateOk),Rejected { message:String } }
pub struct LoopStoreFailure { pub end_reason:LoopEndReason,pub message:String,pub loop_ids:Vec<LoopId> }
pub struct StartFixedRequest { pub original_args:String,pub prompt:String,pub requested_interval:crate::types::RequestedInterval }
pub struct StartDynamicRequest { pub original_args:String,pub prompt:String }
pub struct StartBareRequest { pub original_args:String,pub interval:Option<crate::types::RequestedInterval> }
pub trait LoopController:Send+Sync {
    fn start_fixed(&self,request:StartFixedRequest)->maho_ext_api::ExtensionFuture<'_,LoopCreateOutcome>;
    fn start_dynamic(&self,request:StartDynamicRequest)->maho_ext_api::ExtensionFuture<'_,LoopCreateOutcome>;
    fn start_bare(&self,request:StartBareRequest)->maho_ext_api::ExtensionFuture<'_,LoopCreateOutcome>;
    fn fire_due(&self,loop_id:&str)->maho_ext_api::ExtensionFuture<'_,()>;
    fn schedule_wakeup(&self,request:crate::scheduler::ScheduleWakeupInput)->maho_ext_api::ExtensionFuture<'_,()>;
    fn stop(&self,target:&str,detail:&str)->maho_ext_api::ExtensionFuture<'_,Vec<LoopId>>;
    fn pause(&self,target:&str)->maho_ext_api::ExtensionFuture<'_,Vec<LoopId>>;
    fn resume(&self,target:&str)->maho_ext_api::ExtensionFuture<'_,Vec<LoopId>>;
    fn get_state(&self)->crate::types::LoopState;
    fn status_line(&self)->Option<String>;
    fn last_store_failure(&self)->Option<LoopStoreFailure>;
    fn is_ended_with_error(&self,loop_id:&str)->bool;
}

use std::{collections::BTreeMap,sync::{Arc,atomic::{AtomicU64,Ordering}}};
pub struct NodeTimerPort {
    handles:BTreeMap<String,tokio::task::JoinHandle<()>>,
    generations:BTreeMap<String,Arc<AtomicU64>>,
}
impl Default for NodeTimerPort { fn default()->Self { Self::new() } }
impl NodeTimerPort {
    pub fn new()->Self { Self { handles:BTreeMap::new(),generations:BTreeMap::new() } }
    pub fn cancel(&mut self,key:&str)->Option<tokio::task::JoinHandle<()>> {
        let handle=self.handles.remove(key); if let Some(handle)=&handle { handle.abort(); }
        self.generations.entry(key.into()).or_default().fetch_add(1,Ordering::SeqCst);
        handle
    }
    pub fn arm(&mut self,key:&str,due_at:EpochMs,now:EpochMs,callback:impl FnOnce()+Send+'static)->Option<tokio::task::JoinHandle<()>> {
        let replaced=self.cancel(key);
        let generation=Arc::clone(self.generations.entry(key.into()).or_default());
        let expected=generation.load(Ordering::SeqCst);
        let delay=std::time::Duration::from_secs_f64((due_at-now).max(0.0)/1000.0);
        self.handles.insert(key.into(),tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if generation.load(Ordering::SeqCst)==expected { callback(); }
        }));
        replaced
    }
    pub fn cancel_all(&mut self)->Vec<tokio::task::JoinHandle<()>> {
        self.handles.keys().cloned().collect::<Vec<_>>().iter().filter_map(|key|self.cancel(key)).collect()
    }
}
impl Drop for NodeTimerPort { fn drop(&mut self) { for handle in self.handles.values() { handle.abort(); } } }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn bound_tick_transport_appends_before_followup_and_expands_templates() {
        use maho_ext_api::*;
        struct Capture(std::sync::Mutex<Vec<String>>);
        impl ExtensionActions for Capture {
            fn append_entry(&self,kind:&str,data:Option<JsonValue>)->Result<(),ExtensionFailure> { assert_eq!(kind,LOOP_TICK_ENTRY_TYPE); assert_eq!(data.unwrap()["deliveryId"],"d"); self.0.lock().unwrap().push("entry".into()); Ok(()) }
            fn send_user_message(&self,content:UserMessageContent,options:SendUserMessageOptions)->Result<(),ExtensionFailure> { assert!(matches!(content,UserMessageContent::Text(_))); assert!(options.expand_prompt_templates); assert_eq!(options.deliver_as,Some(StreamingBehavior::FollowUp)); self.0.lock().unwrap().push("message".into()); Ok(()) }
            fn send_message(&self,_:CustomMessage,_:SendMessageOptions)->Result<(),ExtensionFailure> { Err("unexpected custom message".into()) }
            fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure> { Ok(Vec::new()) }
        }
        let capture=Arc::new(Capture(std::sync::Mutex::new(Vec::new()))); let runtime=ExtensionRuntime::default(); runtime.bind(capture.clone());
        let api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
        let prepared=PreparedLoopTick { text:"work".into(),entry:LoopTickEntryData { loop_id:"a".into(),delivery_id:"d".into(),scheduled_for_at:0.0,mode:crate::types::LoopKind::Dynamic,delivery:TickDelivery::Prompt,sentinel:None,noop_streak:0.0,folded:false },delivery_state:Default::default(),defer:false };
        deliver_loop_tick(&api,prepared,true).unwrap(); assert_eq!(*capture.0.lock().unwrap(),vec!["entry","message"]);
    }
    #[test] fn busy_registered_slash_payload_is_deferred_but_plain_prompt_is_not() {
        let mut scheduler=crate::scheduler::LoopScheduler::new("s",None,&Default::default());
        scheduler.create_dynamic(crate::scheduler::CreateDynamicRequest { original_args:"/test".into(),reentry_prompt:"/loop /test".into(),payload:crate::types::LoopPayload::Prompt { prompt:"/test args".into() } },"a".into(),0.0);
        let tick=crate::scheduler::LoopTick { loop_id:"a".into(),delivery_id:"d".into(),scheduled_for_at:0.0,coalesced:false };
        let entry=&scheduler.state.entries["a"]; let commands=vec![maho_ext_api::SlashCommandInfo { name:"test".into(),..Default::default() }];
        assert!(prepare_loop_tick(entry,&tick,Default::default(),crate::tick_prompt::LoopFileSnapshot::Absent,true,&commands).unwrap().defer);
        assert!(!prepare_loop_tick(entry,&tick,Default::default(),crate::tick_prompt::LoopFileSnapshot::Absent,false,&commands).unwrap().defer);
        assert!(!prepare_loop_tick(entry,&tick,Default::default(),crate::tick_prompt::LoopFileSnapshot::Absent,true,&[]).unwrap().defer);
    }
    #[tokio::test] async fn replaced_and_cancelled_callbacks_never_run() {
        let mut timers=NodeTimerPort::new();
        timers.arm("a",0.0,0.0,||panic!("superseded callback ran"));
        let (send,receive)=tokio::sync::oneshot::channel();
        let replaced=timers.arm("a",0.0,0.0,move ||send.send(()).unwrap()).unwrap();
        timers.arm("b",0.0,0.0,||panic!("cancelled callback ran"));
        let cancelled=timers.cancel("b").unwrap();
        assert!(replaced.await.unwrap_err().is_cancelled()); assert!(cancelled.await.unwrap_err().is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(1),receive).await.unwrap().unwrap();
        timers.cancel("a").unwrap().await.unwrap();
    }
    #[tokio::test] async fn cancel_all_observes_every_cancelled_worker() {
        let mut timers=NodeTimerPort::new();
        for key in ["a","b"] { timers.arm(key,1000.0,0.0,||panic!("cancelled callback ran")); }
        let handles=timers.cancel_all(); assert_eq!(handles.len(),2);
        for handle in handles { assert!(handle.await.unwrap_err().is_cancelled()); }
        assert!(timers.cancel_all().is_empty());
    }
}
