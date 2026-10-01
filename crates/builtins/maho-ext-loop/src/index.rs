use crate::{tick_prompt::{LoopMode,TickDelivery},types::{EpochMs,LoopEndReason,LoopId,LoopSentinel,DeliveryId}};
use serde::Serialize;
pub const LOOP_TICK_ENTRY_TYPE:&str="loop-tick";
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
